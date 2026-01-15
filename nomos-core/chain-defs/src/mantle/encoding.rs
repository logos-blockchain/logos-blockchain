use groth16::{CompressedGroth16Proof, Fr, fr_from_bytes};
use key_management_system_keys::keys::{Ed25519Signature, ZkPublicKey, ZkSignature};
use nom::{
    IResult, Parser as _,
    bytes::complete::take,
    combinator::{map, map_res},
    error::{Error, ErrorKind},
    multi::count,
    number::complete::{le_u32, le_u64, u8 as decode_u8},
};

use crate::{
    mantle::{
        MantleTx, Note, NoteId, SignedMantleTx, TxHash,
        ledger::Tx as LedgerTx,
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, Ed25519PublicKey, MsgId, blob::BlobOp, inscribe::InscriptionOp,
                set_keys::SetKeysOp,
            },
            leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier},
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
        },
    },
    sdp::{ActivityMetadata, DeclarationId, Locator, ProviderId, ServiceType},
};

// ==============================================================================
// Top-Level Transaction Decoders
// ==============================================================================

pub fn decode_signed_mantle_tx(input: &[u8]) -> IResult<&[u8], SignedMantleTx> {
    // SignedMantleTx = MantleTx OpsProofs LedgerTxProof
    let (input, mantle_tx) = decode_mantle_tx(input)?;
    let (input, ops_proofs) = decode_ops_proofs(input, &mantle_tx.ops)?;
    let (input, ledger_tx_proof) = decode_zk_signature(input)?;

    let signed_tx = SignedMantleTx::new(mantle_tx, ops_proofs, ledger_tx_proof)
        .map_err(|_| nom::Err::Error(Error::new(input, ErrorKind::Verify)))?;

    Ok((input, signed_tx))
}

pub fn decode_mantle_tx(input: &[u8]) -> IResult<&[u8], MantleTx> {
    // MantleTx = Ops LedgerTx ExecutionGasPrice StorageGasPrice
    let (input, ops) = decode_ops(input)?;
    let (input, ledger_tx) = decode_ledger_tx(input)?;
    let (input, execution_gas_price) = decode_uint64(input)?;
    let (input, storage_gas_price) = decode_uint64(input)?;

    Ok((
        input,
        MantleTx {
            ops,
            ledger_tx,
            execution_gas_price,
            storage_gas_price,
        },
    ))
}

// ==============================================================================
// Operation List Decoders
// ==============================================================================

pub fn decode_ops(input: &[u8]) -> IResult<&[u8], Vec<Op>> {
    // Ops = OpCount *Op
    let (input, op_count) = decode_byte(input)?;
    count(decode_op, op_count as usize).parse(input)
}

pub fn decode_op(input: &[u8]) -> IResult<&[u8], Op> {
    // Op = Opcode OpPayload
    let (input, opcode) = decode_byte(input)?;

    match opcode {
        opcode::INSCRIBE => map(decode_channel_inscribe, Op::ChannelInscribe).parse(input),
        opcode::BLOB => map(decode_channel_blob, Op::ChannelBlob).parse(input),
        opcode::SET_CHANNEL_KEYS => map(decode_channel_set_keys, Op::ChannelSetKeys).parse(input),
        opcode::SDP_DECLARE => map(decode_sdp_declare, Op::SDPDeclare).parse(input),
        opcode::SDP_WITHDRAW => map(decode_sdp_withdraw, Op::SDPWithdraw).parse(input),
        opcode::SDP_ACTIVE => map(decode_sdp_active, Op::SDPActive).parse(input),
        opcode::LEADER_CLAIM => map(decode_leader_claim, Op::LeaderClaim).parse(input),
        _ => Err(nom::Err::Error(Error::new(input, ErrorKind::Fail))),
    }
}

// ==============================================================================
// Channel Operation Decoders
// ==============================================================================

fn decode_channel_inscribe(input: &[u8]) -> IResult<&[u8], InscriptionOp> {
    // ChannelInscribe = ChannelId Inscription Parent Signer
    // Inscription = UINT32 *BYTE
    // Signer = Ed25519PublicKey
    let (input, channel_id) = map(decode_hash32, ChannelId::from).parse(input)?;
    let (input, inscription_len) = decode_uint32(input)?;
    let (input, inscription) =
        map(take(inscription_len as usize), |b: &[u8]| b.to_vec()).parse(input)?;
    let (input, parent) = map(decode_hash32, MsgId::from).parse(input)?;
    let (input, signer) = decode_ed25519_public_key(input)?;

    Ok((
        input,
        InscriptionOp {
            channel_id,
            inscription,
            parent,
            signer,
        },
    ))
}

fn decode_channel_blob(input: &[u8]) -> IResult<&[u8], BlobOp> {
    // ChannelBlob = ChannelId Session BlobId BlobSize DaStorageGasPrice Parent
    // Signer Session = Uint64
    // Signer  = Ed25519PublicKey
    let (input, channel) = map(decode_hash32, ChannelId::from).parse(input)?;
    let (input, session) = decode_uint64(input)?;
    let (input, blob) = decode_hash32(input)?;
    let (input, blob_size) = decode_uint64(input)?;
    let (input, da_storage_gas_price) = decode_uint64(input)?;
    let (input, parent) = map(decode_hash32, MsgId::from).parse(input)?;
    let (input, signer) = decode_ed25519_public_key.parse(input)?;

    Ok((
        input,
        BlobOp {
            channel,
            session,
            blob,
            blob_size,
            da_storage_gas_price,
            parent,
            signer,
        },
    ))
}

fn decode_channel_set_keys(input: &[u8]) -> IResult<&[u8], SetKeysOp> {
    // ChannelSetKeys = ChannelId KeyCount *Ed25519PublicKey
    let (input, channel) = map(decode_hash32, ChannelId::from).parse(input)?;
    let (input, key_count) = decode_byte(input)?;
    let (input, keys) = count(decode_ed25519_public_key, key_count as usize).parse(input)?;

    Ok((input, SetKeysOp { channel, keys }))
}

// ==============================================================================
// SDP Operation Decoders
// ==============================================================================

fn decode_sdp_declare(input: &[u8]) -> IResult<&[u8], SDPDeclareOp> {
    // SDPDeclare = ServiceType LocatorCount *Locator ProviderId ZkId LockedNoteId
    let (input, service_type_byte) = decode_byte(input)?;
    let service_type = match service_type_byte {
        0 => ServiceType::BlendNetwork,
        1 => ServiceType::DataAvailability,
        _ => return Err(nom::Err::Error(Error::new(input, ErrorKind::Fail))),
    };
    let (input, locator_count) = decode_byte(input)?;
    let (input, multiaddrs) = count(decode_locator, locator_count as usize).parse(input)?;
    let locators = multiaddrs.into_iter().map(Locator::new).collect();
    let (input, provider_key) = decode_ed25519_public_key(input)?;
    let provider_id = ProviderId(provider_key);
    let (input, zk_fr) = decode_field_element(input)?;
    let zk_id = ZkPublicKey::new(zk_fr);
    let (input, locked_note_id) = map(decode_field_element, NoteId).parse(input)?;

    Ok((
        input,
        SDPDeclareOp {
            service_type,
            locators,
            provider_id,
            zk_id,
            locked_note_id,
        },
    ))
}

const LOCATOR_BYTES_SIZE_LIMIT: usize = 329usize;

fn decode_locator(input: &[u8]) -> IResult<&[u8], multiaddr::Multiaddr> {
    // Locator = 2Byte *BYTE
    let (input, len_bytes) = take(2usize).parse(input)?;
    let len = u16::from_le_bytes([len_bytes[0], len_bytes[1]]) as usize;
    if len > LOCATOR_BYTES_SIZE_LIMIT {
        return Err(nom::Err::Error(Error::new(input, ErrorKind::LengthValue)));
    }
    map_res(take(len), |bytes: &[u8]| {
        multiaddr::Multiaddr::try_from(bytes.to_vec())
            .map_err(|_| Error::new(bytes, ErrorKind::Fail))
    })
    .parse(input)
}

fn decode_sdp_withdraw(input: &[u8]) -> IResult<&[u8], SDPWithdrawOp> {
    // SDPWithdraw = DeclarationId Nonce LockedNoteId
    let (input, declaration_id_bytes) = decode_hash32(input)?;
    let declaration_id = DeclarationId(declaration_id_bytes);
    let (input, nonce) = decode_uint64(input)?;
    let (input, locked_note_id) = map(decode_field_element, NoteId).parse(input)?;

    Ok((
        input,
        SDPWithdrawOp {
            declaration_id,
            locked_note_id,
            nonce,
        },
    ))
}

fn decode_sdp_active(input: &[u8]) -> IResult<&[u8], SDPActiveOp> {
    // SDPActive = DeclarationId Nonce Metadata
    // Metadata = UINT32 *BYTE
    let (input, declaration_id_bytes) = decode_hash32(input)?;
    let declaration_id = DeclarationId(declaration_id_bytes);

    let (input, nonce) = decode_uint64(input)?;

    let (input, metadata_len) = decode_uint32(input)?;
    let (input, metadata_bytes) = take(metadata_len as usize).parse(input)?;

    let metadata = ActivityMetadata::from_metadata_bytes(metadata_bytes)
        .map_err(|_| nom::Err::Error(Error::new(input, ErrorKind::Fail)))?;

    Ok((
        input,
        SDPActiveOp {
            declaration_id,
            nonce,
            metadata,
        },
    ))
}

// ==============================================================================
// Leader Operation Decoders
// ==============================================================================

fn decode_leader_claim(input: &[u8]) -> IResult<&[u8], LeaderClaimOp> {
    // LeaderClaim = RewardsRoot VoucherNullifier
    let (input, rewards_root_fr) = decode_field_element(input)?;
    let (input, voucher_nullifier_fr) = decode_field_element(input)?;

    Ok((
        input,
        LeaderClaimOp {
            rewards_root: RewardsRoot::from(rewards_root_fr),
            voucher_nullifier: VoucherNullifier::from(voucher_nullifier_fr),
            // The mantle_tx_hash is not part of the wire format per ABNF spec
            // It should be filled in after decoding when the tx hash is computed
            mantle_tx_hash: TxHash::default(),
        },
    ))
}

// ==============================================================================
// Ledger Transaction Decoders
// ==============================================================================

fn decode_note(input: &[u8]) -> IResult<&[u8], Note> {
    // Note = Value ZkPublicKey
    let (input, value) = decode_uint64(input)?;
    let (input, pk) = decode_zk_public_key(input)?;

    Ok((input, Note::new(value, pk)))
}

fn decode_inputs(input: &[u8]) -> IResult<&[u8], Vec<NoteId>> {
    // Inputs = InputCount *NoteId
    let (input, input_count) = decode_byte(input)?;
    count(map(decode_field_element, NoteId), input_count as usize).parse(input)
}

fn decode_outputs(input: &[u8]) -> IResult<&[u8], Vec<Note>> {
    // Outputs = OutputCount *Note
    let (input, output_count) = decode_byte(input)?;
    count(decode_note, output_count as usize).parse(input)
}

fn decode_ledger_tx(input: &[u8]) -> IResult<&[u8], LedgerTx> {
    // LedgerTx = Inputs Outputs
    let (input, inputs) = decode_inputs(input)?;
    let (input, outputs) = decode_outputs(input)?;

    Ok((input, LedgerTx::new(inputs, outputs)))
}

// ==============================================================================
// Proof Decoders
// ==============================================================================

fn decode_ops_proofs<'a>(input: &'a [u8], ops: &[Op]) -> IResult<&'a [u8], Vec<OpProof>> {
    let mut remaining = input;
    let mut proofs = Vec::with_capacity(ops.len());

    for op in ops {
        let (new_remaining, proof) = decode_op_proof(remaining, op)?;
        proofs.push(proof);
        remaining = new_remaining;
    }

    Ok((remaining, proofs))
}

fn decode_op_proof<'a>(input: &'a [u8], op: &Op) -> IResult<&'a [u8], OpProof> {
    match op {
        // Ed25519SigProof = Ed25519Signature
        Op::ChannelInscribe(_) | Op::ChannelBlob(_) | Op::ChannelSetKeys(_) => {
            map(decode_ed25519_signature, OpProof::Ed25519Sig).parse(input)
        }

        // ZkAndEd25519SigsProof = ZkSignature Ed25519Signature
        Op::SDPDeclare(_) => {
            let (input, zk_sig) = decode_zk_signature(input)?;
            let (input, ed25519_sig) = decode_ed25519_signature(input)?;
            Ok((
                input,
                OpProof::ZkAndEd25519Sigs {
                    zk_sig,
                    ed25519_sig,
                },
            ))
        }

        // ZkSigProof = ZkSignature
        Op::SDPWithdraw(_) | Op::SDPActive(_) => {
            map(decode_zk_signature, OpProof::ZkSig).parse(input)
        }

        // ProofOfClaimProof = Groth16
        Op::LeaderClaim(_) => map(decode_groth16, |_proof| {
            panic!("OpProof::LeaderClaimProof not yet implemented");
        })
        .parse(input),
    }
}

// ==============================================================================
// Cryptographic Primitive Decoders
// ==============================================================================

fn decode_zk_signature(input: &[u8]) -> IResult<&[u8], ZkSignature> {
    // ZkSignature = Groth16
    map(decode_groth16, ZkSignature::new).parse(input)
}

const GROTH16_BYTES: usize = 128;
fn decode_groth16(input: &[u8]) -> IResult<&[u8], CompressedGroth16Proof> {
    // Groth16 = 128BYTE
    map(
        decode_array::<GROTH16_BYTES>,
        |proof: [u8; GROTH16_BYTES]| CompressedGroth16Proof::from_bytes(&proof),
    )
    .parse(input)
}

fn decode_zk_public_key(input: &[u8]) -> IResult<&[u8], ZkPublicKey> {
    // ZkPublicKey = FieldElement
    map(decode_field_element, ZkPublicKey::new).parse(input)
}

const ED25519_PK_BYTES: usize = 32;
fn decode_ed25519_public_key(input: &[u8]) -> IResult<&[u8], Ed25519PublicKey> {
    // Ed25519PublicKey = 32BYTE
    map_res(
        decode_array::<ED25519_PK_BYTES>,
        |bytes: [u8; ED25519_PK_BYTES]| {
            Ed25519PublicKey::from_bytes(&bytes).map_err(|_| Error::new(bytes, ErrorKind::Fail))
        },
    )
    .parse(input)
}

const ED25519_SIG_BYTES: usize = 64;
fn decode_ed25519_signature(input: &[u8]) -> IResult<&[u8], Ed25519Signature> {
    // Ed25519Signature = 64BYTE
    map(
        decode_array::<ED25519_SIG_BYTES>,
        |bytes: [u8; ED25519_SIG_BYTES]| Ed25519Signature::from_bytes(&bytes),
    )
    .parse(input)
}

fn decode_field_element(input: &[u8]) -> IResult<&[u8], Fr> {
    // FieldElement = 32BYTE
    map_res(take(32usize), |bytes: &[u8]| {
        fr_from_bytes(bytes).map_err(|_| "Invalid field element")
    })
    .parse(input)
}

fn decode_hash32(input: &[u8]) -> IResult<&[u8], [u8; 32]> {
    // Hash32 = 32BYTE
    decode_array::<32>(input)
}

// ==============================================================================
// Primitive Decoders
// ==============================================================================
fn decode_array<const N: usize>(input: &[u8]) -> IResult<&[u8], [u8; N]> {
    map(take(N), |bytes: &[u8]| {
        let mut arr = [0u8; N];
        arr.copy_from_slice(bytes);
        arr
    })
    .parse(input)
}

fn decode_uint64(input: &[u8]) -> IResult<&[u8], u64> {
    // UINT64 = 8BYTE
    le_u64(input)
}

fn decode_uint32(input: &[u8]) -> IResult<&[u8], u32> {
    // UINT32 = 4BYTE
    le_u32(input)
}

fn decode_byte(input: &[u8]) -> IResult<&[u8], u8> {
    // Byte = OCTET
    decode_u8(input)
}

// ==============================================================================
// Binary Encoders
// ==============================================================================

use groth16::fr_to_bytes;

use super::ops::opcode;

/// Encode primitives
fn encode_uint64(value: u64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn encode_uint32(value: u32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn encode_byte(value: u8) -> Vec<u8> {
    vec![value]
}

fn encode_hash32(hash: &[u8; 32]) -> Vec<u8> {
    hash.to_vec()
}

fn encode_field_element(fr: &Fr) -> Vec<u8> {
    fr_to_bytes(fr).to_vec()
}

/// Encode cryptographic primitives
fn encode_ed25519_signature(sig: &Ed25519Signature) -> Vec<u8> {
    sig.to_bytes().to_vec()
}

fn encode_ed25519_public_key(key: &Ed25519PublicKey) -> Vec<u8> {
    key.to_bytes().to_vec()
}

fn encode_zk_signature(sig: &ZkSignature) -> Vec<u8> {
    // ZkSignature wraps ZkSignProof which is CompressedGroth16Proof
    // CompressedProof is 128 bytes: pi_a (32) + pi_b (64) + pi_c (32)
    sig.as_proof().to_bytes().to_vec()
}

/// Encode channel operations
#[must_use]
pub fn encode_channel_inscribe(op: &InscriptionOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(op.channel_id.as_ref()));
    bytes.extend(encode_uint32(op.inscription.len() as u32));
    bytes.extend(&op.inscription);
    bytes.extend(encode_hash32(op.parent.as_ref()));
    bytes.extend(encode_ed25519_public_key(&op.signer));
    bytes
}

#[must_use]
pub fn encode_channel_blob(op: &BlobOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(op.channel.as_ref()));
    bytes.extend(encode_uint64(op.session));
    bytes.extend(encode_hash32(&op.blob));
    bytes.extend(encode_uint64(op.blob_size));
    bytes.extend(encode_uint64(op.da_storage_gas_price));
    bytes.extend(encode_hash32(op.parent.as_ref()));
    bytes.extend(encode_ed25519_public_key(&op.signer));
    bytes
}

fn encode_channel_set_keys(op: &SetKeysOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(op.channel.as_ref()));
    bytes.extend(encode_byte(op.keys.len() as u8));
    for key in &op.keys {
        bytes.extend(encode_ed25519_public_key(key));
    }
    bytes
}

/// Encode SDP operations
fn encode_locator(locator: &multiaddr::Multiaddr) -> Vec<u8> {
    let locator_bytes = locator.to_vec();
    assert!(locator_bytes.len() <= LOCATOR_BYTES_SIZE_LIMIT);
    let mut bytes = Vec::new();
    bytes.extend((locator_bytes.len() as u16).to_le_bytes());
    bytes.extend(locator_bytes);
    bytes
}

fn encode_sdp_declare(op: &SDPDeclareOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    // ServiceType
    let service_type_byte = match op.service_type {
        ServiceType::BlendNetwork => 0u8,
        ServiceType::DataAvailability => 1u8,
    };
    bytes.extend(encode_byte(service_type_byte));
    // Locators
    bytes.extend(encode_byte(op.locators.len() as u8));
    for locator in &op.locators {
        bytes.extend(encode_locator(locator.as_ref()));
    }
    // ProviderId
    bytes.extend(encode_ed25519_public_key(&op.provider_id.0));
    // ZkId
    bytes.extend(encode_field_element(op.zk_id.as_fr()));
    // LockedNoteId
    bytes.extend(encode_field_element(op.locked_note_id.as_ref()));
    bytes
}

fn encode_sdp_withdraw(op: &SDPWithdrawOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(&op.declaration_id.0));
    bytes.extend(encode_uint64(op.nonce));
    bytes.extend(encode_field_element(op.locked_note_id.as_ref()));
    bytes
}

#[must_use]
pub fn encode_sdp_active(op: &SDPActiveOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(&op.declaration_id.0));
    bytes.extend(encode_uint64(op.nonce));

    // Metadata - convert ActivityMetadata to bytes
    let metadata_bytes = op.metadata.to_metadata_bytes();

    bytes.extend(encode_uint32(metadata_bytes.len() as u32));
    bytes.extend(&metadata_bytes);
    bytes
}

/// Encode leader operations
fn encode_leader_claim(op: &LeaderClaimOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field_element(&op.rewards_root.into()));
    bytes.extend(encode_field_element(&op.voucher_nullifier.into()));
    bytes
}

/// Encode ledger transactions
fn encode_note(note: &Note) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_uint64(note.value));
    bytes.extend(encode_field_element(note.pk.as_fr()));
    bytes
}

fn encode_inputs(inputs: &[NoteId]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_byte(inputs.len() as u8));
    for input in inputs {
        bytes.extend(encode_field_element(input.as_ref()));
    }
    bytes
}

fn encode_outputs(outputs: &[Note]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_byte(outputs.len() as u8));
    for output in outputs {
        bytes.extend(encode_note(output));
    }
    bytes
}

#[must_use]
pub fn encode_ledger_tx(tx: &LedgerTx) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_inputs(&tx.inputs));
    bytes.extend(encode_outputs(&tx.outputs));
    bytes
}

/// Encode operations
#[must_use]
pub fn encode_op(op: &Op) -> Vec<u8> {
    let mut bytes = Vec::new();
    match op {
        Op::ChannelInscribe(op) => {
            bytes.extend(encode_byte(opcode::INSCRIBE));
            bytes.extend(encode_channel_inscribe(op));
        }
        Op::ChannelBlob(op) => {
            bytes.extend(encode_byte(opcode::BLOB));
            bytes.extend(encode_channel_blob(op));
        }
        Op::ChannelSetKeys(op) => {
            bytes.extend(encode_byte(opcode::SET_CHANNEL_KEYS));
            bytes.extend(encode_channel_set_keys(op));
        }
        Op::SDPDeclare(op) => {
            bytes.extend(encode_byte(opcode::SDP_DECLARE));
            bytes.extend(encode_sdp_declare(op));
        }
        Op::SDPWithdraw(op) => {
            bytes.extend(encode_byte(opcode::SDP_WITHDRAW));
            bytes.extend(encode_sdp_withdraw(op));
        }
        Op::SDPActive(op) => {
            bytes.extend(encode_byte(opcode::SDP_ACTIVE));
            bytes.extend(encode_sdp_active(op));
        }
        Op::LeaderClaim(op) => {
            bytes.extend(encode_byte(opcode::LEADER_CLAIM));
            bytes.extend(encode_leader_claim(op));
        }
    }
    bytes
}

fn encode_ops(ops: &[Op]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_byte(ops.len() as u8));
    for op in ops {
        bytes.extend(encode_op(op));
    }
    bytes
}

/// Encode proofs
fn encode_op_proof(proof: &OpProof, op: &Op) -> Vec<u8> {
    match (proof, op) {
        (
            OpProof::Ed25519Sig(sig),
            Op::ChannelInscribe(_) | Op::ChannelBlob(_) | Op::ChannelSetKeys(_),
        ) => encode_ed25519_signature(sig),
        (
            OpProof::ZkAndEd25519Sigs {
                zk_sig,
                ed25519_sig,
            },
            Op::SDPDeclare(_),
        ) => {
            let mut bytes = encode_zk_signature(zk_sig);
            bytes.extend(encode_ed25519_signature(ed25519_sig));
            bytes
        }
        (OpProof::ZkSig(sig), Op::SDPWithdraw(_) | Op::SDPActive(_)) => encode_zk_signature(sig),
        (_, Op::LeaderClaim(_)) => {
            unimplemented!("ProofOfClaimProof not implemented");
        }
        _ => {
            panic!("Mismatch between proof type and operation type");
        }
    }
}

fn encode_ops_proofs(proofs: &[OpProof], ops: &[Op]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (proof, op) in proofs.iter().zip(ops.iter()) {
        bytes.extend(encode_op_proof(proof, op));
    }
    bytes
}

/// Encode top-level transactions
#[must_use]
pub fn encode_mantle_tx(tx: &MantleTx) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_ops(&tx.ops));
    bytes.extend(encode_ledger_tx(&tx.ledger_tx));
    bytes.extend(encode_uint64(tx.execution_gas_price));
    bytes.extend(encode_uint64(tx.storage_gas_price));
    bytes
}

#[must_use]
pub fn encode_signed_mantle_tx(tx: &SignedMantleTx) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_mantle_tx(&tx.mantle_tx));
    bytes.extend(encode_ops_proofs(&tx.ops_proofs, &tx.mantle_tx.ops));
    bytes.extend(encode_zk_signature(&tx.ledger_tx_proof));
    bytes
}

pub(crate) fn predict_signed_mantle_tx_size(tx: &MantleTx) -> usize {
    let mantle_tx_size = encode_mantle_tx(tx).len();

    let ops_proofs_size = tx
        .ops
        .iter()
        .map(|op| match op {
            // Ed25519SigProof = Ed25519Signature
            Op::ChannelInscribe(_) | Op::ChannelBlob(_) | Op::ChannelSetKeys(_) => {
                ED25519_SIG_BYTES
            }

            // ZkAndEd25519SigsProof = ZkSignature Ed25519Signature
            Op::SDPDeclare(_) => GROTH16_BYTES + ED25519_SIG_BYTES,

            // ZkSigProof  = ZkSignature
            Op::SDPWithdraw(_) | Op::SDPActive(_) => GROTH16_BYTES,

            // ProofOfClaimProof = Groth16
            Op::LeaderClaim(_) => {
                unimplemented!("OpProof::LeaderClaimProof not yet implemented");
            }
        })
        .sum::<usize>();

    // LedgerTxProof = ZkSignature
    // ZkSignature   = Groth16
    let ledger_tx_proof_size = GROTH16_BYTES;

    mantle_tx_size + ops_proofs_size + ledger_tx_proof_size
}

#[cfg(test)]
mod tests {
    use ark_ff::Field as _;
    use key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::{mantle::Transaction as _, sdp::da::ActivityProof};

    fn dbg_test_vector(actual: &str, expected: &str) {
        println!("{:32} {:32}", "actual", "expected");
        for (actual_chunk, expected_chunk) in actual
            .chars()
            .collect::<Vec<_>>()
            .chunks(32)
            .map(String::from_iter)
            .zip(
                expected
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(32)
                    .map(String::from_iter),
            )
        {
            println!(
                "{actual_chunk:32} {expected_chunk:32} {}",
                if actual_chunk == expected_chunk {
                    "same"
                } else {
                    "different"
                }
            );
        }
    }

    fn zksig(sig_hex: &str) -> ZkSignature {
        // zksign signatures are non-deterministic meaning we can't simply regenerate
        // the proofs in tests on each run.
        // This utility allows us to hardcode the sig in tests as hex.
        assert_eq!(sig_hex.len(), 256); // each byte takes two chars in hex;

        let mut sig_bytes = [0u8; 128];
        hex::decode_to_slice(sig_hex, &mut sig_bytes).unwrap();

        ZkSignature::new(CompressedGroth16Proof::from_bytes(&sig_bytes))
    }

    #[test]
    fn test_decode_primitives() {
        // Test UINT64
        let data = 42u64.to_le_bytes();
        let (remaining, value) = decode_uint64(&data).unwrap();
        assert_eq!(value, 42u64);
        assert!(remaining.is_empty());

        // Test UINT32
        let data = 123u32.to_le_bytes();
        let (remaining, value) = decode_uint32(&data).unwrap();
        assert_eq!(value, 123u32);
        assert!(remaining.is_empty());

        // Test Byte
        let data = [0xAB];
        let (remaining, value) = decode_byte(&data).unwrap();
        assert_eq!(value, 0xAB);
        assert!(remaining.is_empty());

        // Test Hash32
        let data = [0x42u8; 32];
        let (remaining, value) = decode_hash32(&data).unwrap();
        assert_eq!(value, [0x42u8; 32]);
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_decode_signed_mantle_tx_empty() {
        let mantle_tx = MantleTx {
            ops: vec![],
            ledger_tx: LedgerTx {
                inputs: vec![],
                outputs: vec![],
            },
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        let ledger_tx_proof = zksig(
            // hex::encode(&ZkKey::multi_sign([], &txhash.0))
            "fcdf9c12b2871b271f64f39722ce0f5ff1809d5f61e11233387d9b04af2c1da2bb61b193d57333661e4c6151c6c35b999ee1ab6fb957658511f19887256ef71cc13cda86473ef9c4af10b2c31eb714b50177d68ca185c37779b3de83c78e5bb048e2d8da6ae97eb1d51514e0df379ff72f14175121c1b07f3affe85206a1d992",
        );
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![],
            ledger_tx_proof,
        };

        #[expect(
            clippy::string_add,
            reason = "Recommended String::push_str does not support chaining"
        )]
        let test_vector = String::new()
            + "00"                                                               // OpCount=0u8
            + "00"                                                               // LedgerInputCount=0u8
            + "00"                                                               // LedgerOutputCount=0u8
            + "6400000000000000"                                                 // ExecutionGasPrice
            + "3200000000000000"                                                 // StorageGasPrice
            + "fcdf9c12b2871b271f64f39722ce0f5ff1809d5f61e11233387d9b04af2c1da2" // ZkSignature (128Byte)
            + "bb61b193d57333661e4c6151c6c35b999ee1ab6fb957658511f19887256ef71c"
            + "c13cda86473ef9c4af10b2c31eb714b50177d68ca185c37779b3de83c78e5bb0"
            + "48e2d8da6ae97eb1d51514e0df379ff72f14175121c1b07f3affe85206a1d992";

        // ENCODING
        let encoded = hex::encode(encode_signed_mantle_tx(&signed_tx));
        if encoded != test_vector {
            dbg_test_vector(&encoded, &test_vector);
            assert_eq!(encoded, test_vector);
        }

        // DECODING
        let test_vector_bytes = hex::decode(test_vector).unwrap();
        let (remaining, decoded_tx) = decode_signed_mantle_tx(&test_vector_bytes).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(decoded_tx, signed_tx);
    }

    #[test]
    fn test_decode_signed_mantle_tx_with_inscribe() {
        let signing_key = Ed25519Key::from_bytes(&[4u8; 32]);
        let mantle_tx = MantleTx {
            ops: vec![Op::ChannelInscribe(InscriptionOp {
                channel_id: ChannelId::from([0xAA; 32]),
                inscription: b"hello".to_vec(),
                parent: MsgId::from([0xBB; 32]),
                signer: signing_key.public_key(),
            })],
            ledger_tx: LedgerTx {
                inputs: vec![],
                outputs: vec![],
            },
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        let txhash = mantle_tx.hash();
        let inscribe_sig =
            OpProof::Ed25519Sig(signing_key.sign_payload(&txhash.as_signing_bytes()));
        let ledger_tx_proof = zksig(
            // ZkKey::multi_sign([], txhash.as_ref())
            "f8bdd66cbbbae6cba142f2c15ccc8b0c3cb10566e7ca89978ef987515f922c95ef2c897d66d12352fcbf7657da8cec24a3e8a6b9338278b0e7be953be416ce2510b53711585e78e1e4d402f7348f72adc134608a520e8b7ec5dad75b287f14a51836b52db2760aba14e4a3cc820f5393a97595a06403d8aac284bf4e8cf85d99",
        );
        let signed_tx =
            SignedMantleTx::new(mantle_tx, vec![inscribe_sig], ledger_tx_proof).unwrap();

        #[expect(
            clippy::string_add,
            reason = "Recommended String::push_str does not support chaining"
        )]
        let test_vector = String::new()
            + "01"                                                               // OpCount
            + "00"                                                               // OpCode
            + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" // ChannelID (32Byte)
            + "05000000"                                                         // InscriptionLength
            + "68656c6c6f"                                                       // Inscription
            + "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" // Parent (32Byte)
            + "ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c" // Signer (32Byte)
            + "00"                                                               // LedgerInputCount
            + "00"                                                               // LedgerOutputCount
            + "6400000000000000"                                                 // ExecutionGasPrice
            + "3200000000000000"                                                 // StorageGasPrice
            + "e0195c329ab39c18b05b7e226d1ecea2f3dc40ac2f4fa3eb4caf98c9563a6255" // Signature (64Byte)
            + "25a9e5048368ea31d50c74da105d41ad3723c001371553e3da1bbe762859ed0b"
            + "f8bdd66cbbbae6cba142f2c15ccc8b0c3cb10566e7ca89978ef987515f922c95" // ZkSignature (128Byte)
            + "ef2c897d66d12352fcbf7657da8cec24a3e8a6b9338278b0e7be953be416ce25"
            + "10b53711585e78e1e4d402f7348f72adc134608a520e8b7ec5dad75b287f14a5"
            + "1836b52db2760aba14e4a3cc820f5393a97595a06403d8aac284bf4e8cf85d99";

        // ENCODING
        let encoded = hex::encode(encode_signed_mantle_tx(&signed_tx));
        if encoded != test_vector {
            dbg_test_vector(&encoded, &test_vector);
            assert_eq!(encoded, test_vector);
        }

        // DECODING
        let test_vector_bytes = hex::decode(test_vector).unwrap();
        let (remaining, decoded_tx) = decode_signed_mantle_tx(&test_vector_bytes).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(decoded_tx, signed_tx);
    }
    #[test]
    fn test_decode_signed_mantle_tx_with_blob() {
        let signing_key = Ed25519Key::from_bytes(&[4u8; 32]);
        let mantle_tx = MantleTx {
            ops: vec![Op::ChannelBlob(BlobOp {
                channel: ChannelId::from([0xAA; 32]),
                session: 0u64,
                blob: [0xBB; 32],
                blob_size: 1024,
                da_storage_gas_price: 10,
                parent: MsgId::from([0x00; 32]),
                signer: signing_key.public_key(),
            })],
            ledger_tx: LedgerTx {
                inputs: vec![],
                outputs: vec![],
            },
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        let txhash = mantle_tx.hash();
        let blob_sig = OpProof::Ed25519Sig(signing_key.sign_payload(&txhash.as_signing_bytes()));
        let ledger_tx_proof = zksig(
            // ZkKey::multi_sign([], &txhash.0)
            "e05fa8a516f4b3bdc2aa85938c47702ef2dddbdf480217c6e262eece511758299321cff6aa14d050ef653c83ab20f939d3c05227f4be58973ee6c7618ba79c09c80e560c8354a4b87d3041407472b32708fdf9119094323f0d6ecd5d2ebea297c6b2b3eb2728c0c7fb30123d0087eb1bae8e29f6cf31371c0fa46de474a50292",
        );
        let signed_tx = SignedMantleTx::new(mantle_tx, vec![blob_sig], ledger_tx_proof).unwrap();

        #[expect(
            clippy::string_add,
            reason = "Recommended String::push_str does not support chaining"
        )]
        let test_vector = String::new()
            + "01"                                                               // OpCount
            + "01"                                                               // OpCode
            + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" // ChannelID (32Byte)
            + "0000000000000000"                                                 // SessionNumber (u64)
            + "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" // BlobId (32Byte)
            + "0004000000000000"                                                 // BlobSize (u64)
            + "0a00000000000000"                                                 // DaStorageGasPrice (u64)
            + "0000000000000000000000000000000000000000000000000000000000000000" // Parent (32Byte)
            + "ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c" // Signer (32Byte)
            + "00"                                                               // LedgerInputCount
            + "00"                                                               // LedgerOutputCount
            + "6400000000000000"                                                 // ExecutionGasPrice
            + "3200000000000000"                                                 // StorageGasPrice
            + "2a3f65e717a4ed8e6cc44c7b5fd6fd6a6d97267e590b434f37fac876254444df" // Ed25519Signature (64Byte)
            + "ebbe1e01350551fa54b8ed840dec1f060ee75d377876ccc6bf708a118d376402"
            + "e05fa8a516f4b3bdc2aa85938c47702ef2dddbdf480217c6e262eece51175829" // ZkSignature (128Byte)
            + "9321cff6aa14d050ef653c83ab20f939d3c05227f4be58973ee6c7618ba79c09"
            + "c80e560c8354a4b87d3041407472b32708fdf9119094323f0d6ecd5d2ebea297"
            + "c6b2b3eb2728c0c7fb30123d0087eb1bae8e29f6cf31371c0fa46de474a50292";

        // ENCODING
        let encoded = hex::encode(encode_signed_mantle_tx(&signed_tx));
        if encoded != test_vector {
            dbg_test_vector(&encoded, &test_vector);
            assert_eq!(encoded, test_vector);
        }

        // DECODING
        let test_vector_bytes = hex::decode(test_vector).unwrap();
        let (remaining, decoded_tx) = decode_signed_mantle_tx(&test_vector_bytes).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(decoded_tx, signed_tx);
    }

    #[test]
    fn test_decode_signed_mantle_tx_with_multiple_ops() {
        let signing_key = Ed25519Key::from_bytes(&[4u8; 32]);
        let mantle_tx = MantleTx {
            ops: vec![
                Op::ChannelInscribe(InscriptionOp {
                    channel_id: ChannelId::from([0x11; 32]),
                    inscription: b"first".to_vec(),
                    parent: MsgId::from([0x00; 32]),
                    signer: signing_key.public_key(),
                }),
                Op::ChannelBlob(BlobOp {
                    channel: ChannelId::from([0x22; 32]),
                    session: 0u64,
                    blob: [0x33; 32],
                    blob_size: 2048,
                    da_storage_gas_price: 20,
                    parent: MsgId::from([0x44; 32]),
                    signer: signing_key.public_key(),
                }),
            ],
            ledger_tx: LedgerTx {
                inputs: vec![],
                outputs: vec![],
            },
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        let txhash = mantle_tx.hash();
        let sig = signing_key.sign_payload(&txhash.as_signing_bytes());
        let ledger_tx_proof = zksig(
            // ZkKey::multi_sign([], &txhash.0)
            "bf5fac329532b4c08784494945535887808607fca0b3b354e6303e03e58d4a966d1fc5818d2955a3b20d5cb38d93a9afa752035052b956b2617e61c495f0ce2ebb33fd9c546dfd507aeceb360bde13c882cf475a814a9c799b0d7a2519541212f3208080e89b8d8401e4b09fdb433f9186b6565bca27c42f89454ffee743c203",
        );
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(sig), OpProof::Ed25519Sig(sig)],
            ledger_tx_proof,
        )
        .unwrap();

        #[expect(
            clippy::string_add,
            reason = "Recommended String::push_str does not support chaining"
        )]
        let test_vector = String::new()
            + "02"                                                               // OpCount=2
            + "00"                                                               // Op1: OpCode=ChannelInscribe
            + "1111111111111111111111111111111111111111111111111111111111111111" // Op1: ChannelId (32Byte)
            + "05000000"                                                         // Op1: InscriptionLength=5u32
            + "6669727374"                                                       // Op1: Inscription="first"
            + "0000000000000000000000000000000000000000000000000000000000000000" // Op1: Parent (32Byte)
            + "ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c" // Op1: Signer (32Byte)
            + "01"                                                               // Op2: OpCode=ChannelBlob
            + "2222222222222222222222222222222222222222222222222222222222222222" // Op2: ChannelId (32Byte)
            + "0000000000000000"                                                 // Op2: SessionNumber (u64)
            + "3333333333333333333333333333333333333333333333333333333333333333" // Op2: BlobId (32Byte)
            + "0008000000000000"                                                 // Op2: BlobSize=2048u64
            + "1400000000000000"                                                 // Op2: DaStorageGasPrice=20u64
            + "4444444444444444444444444444444444444444444444444444444444444444" // Op2: Parent (32Byte)
            + "ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c" // Op2: Signer (32Byte)
            + "00"                                                               // LedgerInputCount
            + "00"                                                               // LedgerOutputCount
            + "6400000000000000"                                                 // ExecutionGasPrice
            + "3200000000000000"                                                 // StorageGasPrice
            + "8872fb1dfe7d5aee9b95cb8bc8ae4d2a3482a97adf4dab57f32b4356199cefa6" // Ed25519Signature (64Byte)
            + "9de99d0dcecc0a15a5fcdf84e57afdb841987412407c43f4898eecc4debfe301" // -- for Op1
            + "8872fb1dfe7d5aee9b95cb8bc8ae4d2a3482a97adf4dab57f32b4356199cefa6" // Ed25519Signature (64Byte)
            + "9de99d0dcecc0a15a5fcdf84e57afdb841987412407c43f4898eecc4debfe301" // -- for Op2
            + "bf5fac329532b4c08784494945535887808607fca0b3b354e6303e03e58d4a96" // ZkSignature (128Byte)
            + "6d1fc5818d2955a3b20d5cb38d93a9afa752035052b956b2617e61c495f0ce2e" // -- for ledger tx
            + "bb33fd9c546dfd507aeceb360bde13c882cf475a814a9c799b0d7a2519541212"
            + "f3208080e89b8d8401e4b09fdb433f9186b6565bca27c42f89454ffee743c203";

        // ENCODING
        let encoded = hex::encode(encode_signed_mantle_tx(&signed_tx));
        if encoded != test_vector {
            dbg_test_vector(&encoded, &test_vector);
            assert_eq!(encoded, test_vector);
        }

        // DECODING
        let test_vector_bytes = hex::decode(test_vector).unwrap();
        let (remaining, decoded_tx) = decode_signed_mantle_tx(&test_vector_bytes).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(decoded_tx, signed_tx);
    }

    #[test]
    fn test_encode_decode_roundtrip_empty_tx() {
        // Create an empty MantleTx
        let original_tx = MantleTx {
            ops: vec![],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Encode
        let encoded = encode_mantle_tx(&original_tx);

        // Decode
        let (remaining, decoded_tx) = decode_mantle_tx(&encoded).unwrap();

        // Verify
        assert!(remaining.is_empty());
        assert_eq!(original_tx, decoded_tx);
    }

    #[test]
    fn test_encode_decode_roundtrip_with_ledger_tx() {
        use num_bigint::BigUint;

        // Create a MantleTx with ledger inputs and outputs
        let pk = ZkPublicKey::from(BigUint::from(42u64));
        let note = Note::new(1000, pk);
        let note_id = NoteId(BigUint::from(123u64).into());

        let original_tx = MantleTx {
            ops: vec![],
            ledger_tx: LedgerTx::new(vec![note_id], vec![note]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Encode
        let encoded = encode_mantle_tx(&original_tx);

        // Decode
        let (remaining, decoded_tx) = decode_mantle_tx(&encoded).unwrap();

        // Verify
        assert!(remaining.is_empty());
        assert_eq!(original_tx, decoded_tx);
    }

    #[test]
    fn test_encode_decode_roundtrip_signed_tx() {
        // Create a simple SignedMantleTx
        let mantle_tx = MantleTx {
            ops: vec![],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };
        let ledger_tx_proof = ZkKey::multi_sign(&[], mantle_tx.hash().as_ref()).unwrap();
        let original_tx = SignedMantleTx::new(mantle_tx, vec![], ledger_tx_proof).unwrap();

        // Encode
        let encoded = encode_signed_mantle_tx(&original_tx);

        // Decode
        let (remaining, decoded_tx) = decode_signed_mantle_tx(&encoded).unwrap();

        // Verify
        assert!(remaining.is_empty());
        assert_eq!(original_tx, decoded_tx);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_empty_tx() {
        // Create an empty MantleTx
        let mantle_tx = MantleTx {
            ops: vec![],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        // Create a signed tx and encode it to get actual size
        let txhash = mantle_tx.hash();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_inscribe() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"hello world".to_vec(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        };

        let mantle_tx = MantleTx {
            ops: vec![Op::ChannelInscribe(inscribe_op)],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        // Create a signed tx and encode it to get actual size
        let txhash = mantle_tx.hash();
        let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(op_sig)],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_blob() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let blob_op = BlobOp {
            channel: ChannelId::from([0xCC; 32]),
            session: 0u64,
            blob: [0xDD; 32],
            blob_size: 1024,
            da_storage_gas_price: 10,
            parent: MsgId::from([0xEE; 32]),
            signer: signing_key.public_key(),
        };

        let mantle_tx = MantleTx {
            ops: vec![Op::ChannelBlob(blob_op)],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        let txhash = mantle_tx.hash();
        // Create a signed tx and encode it to get actual size
        let blob_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(blob_sig)],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_set_keys() {
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);
        let signing_key3 = Ed25519Key::from_bytes(&[3; 32]);

        let set_keys_op = SetKeysOp {
            channel: ChannelId::from([0xFF; 32]),
            keys: vec![
                signing_key1.public_key(),
                signing_key2.public_key(),
                signing_key3.public_key(),
            ],
        };

        let mantle_tx = MantleTx {
            ops: vec![Op::ChannelSetKeys(set_keys_op)],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        // Create a signed tx and encode it to get actual size
        let dummy_ed25519_sig = Ed25519Signature::from_bytes(&[0; 64]);
        let txhash = mantle_tx.hash();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(dummy_ed25519_sig)],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_sdp_declare() {
        use num_bigint::BigUint;

        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let zk_sk = ZkKey::zero();
        let locator1: multiaddr::Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        let locator2: multiaddr::Multiaddr = "/ip6/::1/tcp/9090".parse().unwrap();

        let locked_note_sk = ZkKey::from(BigUint::from(1u64));
        let locked_note = crate::mantle::Utxo {
            tx_hash: TxHash::from(BigUint::from(42u64)),
            output_index: 12,
            note: Note {
                value: 500,
                pk: locked_note_sk.to_public_key(),
            },
        };
        let sdp_declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec![Locator::new(locator1), Locator::new(locator2)],
            provider_id: ProviderId(signing_key.public_key()),
            zk_id: zk_sk.to_public_key(),
            locked_note_id: locked_note.id(),
        };

        let mantle_tx = MantleTx {
            ops: vec![Op::SDPDeclare(sdp_declare_op)],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        // Create a signed tx and encode it to get actual size
        let txhash = mantle_tx.hash();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkAndEd25519Sigs {
                zk_sig: ZkKey::multi_sign(&[locked_note_sk, zk_sk], &txhash.0).unwrap(),
                ed25519_sig: Ed25519Signature::from_bytes(&[0u8; 64]),
            }],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_sdp_withdraw() {
        let locked_note_id = NoteId(BigUint::from(123u64).into());

        let sdp_withdraw_op = SDPWithdrawOp {
            declaration_id: DeclarationId([0x11; 32]),
            nonce: 42,
            locked_note_id,
        };

        let mantle_tx = MantleTx {
            ops: vec![Op::SDPWithdraw(sdp_withdraw_op)],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        let txhash = mantle_tx.hash();

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        // Create a signed tx and encode it to get actual size
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkSig(
                ZkKey::multi_sign(&[ZkKey::zero()], &txhash.0).unwrap(),
            )],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_sdp_active() {
        let da_proof = ActivityProof {
            current_session: 42,
            previous_session_opinions: vec![0xFF, 0xAA],
            current_session_opinions: vec![0x11, 0x22, 0x33],
        };

        let metadata = ActivityMetadata::DataAvailability(da_proof);

        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x22; 32]),
            nonce: 99,
            metadata,
        };

        let mantle_tx = MantleTx {
            ops: vec![Op::SDPActive(sdp_active_op)],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        let txhash = mantle_tx.hash();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkSig(
                ZkKey::multi_sign(&[ZkKey::zero()], &txhash.0).unwrap(),
            )],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();

        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_multiple_ops() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"test".to_vec(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        };

        let blob_op = BlobOp {
            channel: ChannelId::from([0xCC; 32]),
            session: 0u64,
            blob: [0xDD; 32],
            blob_size: 2048,
            da_storage_gas_price: 20,
            parent: MsgId::from([0xEE; 32]),
            signer: signing_key.public_key(),
        };

        let proof = ActivityProof {
            current_session: u64::MAX,
            previous_session_opinions: vec![0xFF; 1000],
            current_session_opinions: vec![0xAA; 2000],
        };

        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x33; 32]),
            nonce: 55,
            metadata: ActivityMetadata::DataAvailability(proof),
        };

        let mantle_tx = MantleTx {
            ops: vec![
                Op::ChannelInscribe(inscribe_op),
                Op::ChannelBlob(blob_op),
                Op::SDPActive(sdp_active_op),
            ],
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        let txhash = mantle_tx.hash();
        let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
        // Create a signed tx and encode it to get actual size
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(op_sig),
                OpProof::Ed25519Sig(op_sig),
                OpProof::ZkSig(ZkKey::zero().sign_payload(&txhash.0).unwrap()),
            ],
            ZkKey::multi_sign(&[], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_ledger_inputs_outputs() {
        use num_bigint::BigUint;

        let pk1 = ZkPublicKey::from(BigUint::from(100u64));
        let pk2 = ZkPublicKey::from(BigUint::from(200u64));

        let note1 = Note::new(1000, pk1);
        let note2 = Note::new(2000, pk2);

        let note_id1 = NoteId(BigUint::from(111u64).into());
        let note_id2 = NoteId(BigUint::from(222u64).into());
        let note_id3 = NoteId(BigUint::from(333u64).into());

        let mantle_tx = MantleTx {
            ops: vec![],
            ledger_tx: LedgerTx::new(vec![note_id1, note_id2, note_id3], vec![note1, note2]),
            execution_gas_price: 100,
            storage_gas_price: 50,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        // Create a signed tx and encode it to get actual size
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![],
            ZkKey::multi_sign(&[], &Fr::ZERO).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_complex_scenario() {
        use num_bigint::BigUint;

        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0x11; 32]),
            inscription: b"complex test inscription with more data".to_vec(),
            parent: MsgId::from([0x22; 32]),
            signer: signing_key1.public_key(),
        };

        let set_keys_op = SetKeysOp {
            channel: ChannelId::from([0x33; 32]),
            keys: vec![signing_key1.public_key(), signing_key2.public_key()],
        };

        let locked_note_sk = ZkKey::from(BigUint::from(1u64));
        let ledger_tx = LedgerTx {
            inputs: vec![NoteId(BigUint::from(777u64).into())],
            outputs: vec![Note::new(5000, locked_note_sk.to_public_key())],
        };

        let locator: multiaddr::Multiaddr = "/dns4/example.com/tcp/443".parse().unwrap();
        let zk_sk = ZkKey::zero();
        let sdp_declare_op = SDPDeclareOp {
            service_type: ServiceType::DataAvailability,
            locators: vec![Locator::new(locator)],
            provider_id: ProviderId(signing_key1.public_key()),
            zk_id: zk_sk.to_public_key(),
            locked_note_id: ledger_tx.utxo_by_index(0).unwrap().id(),
        };

        let mantle_tx = MantleTx {
            ops: vec![
                Op::ChannelInscribe(inscribe_op),
                Op::ChannelSetKeys(set_keys_op),
                Op::SDPDeclare(sdp_declare_op),
            ],
            ledger_tx,
            execution_gas_price: 150,
            storage_gas_price: 75,
        };

        // Predict size
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx);

        // Create a signed tx and encode it to get actual size
        let txhash = mantle_tx.hash();
        let op_ed25519_sig = signing_key1.sign_payload(&txhash.as_signing_bytes());
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(op_ed25519_sig),
                OpProof::Ed25519Sig(op_ed25519_sig),
                OpProof::ZkAndEd25519Sigs {
                    zk_sig: ZkKey::multi_sign(&[locked_note_sk, zk_sk], &txhash.0).unwrap(),
                    ed25519_sig: op_ed25519_sig,
                },
            ],
            ZkKey::multi_sign(&[ZkKey::zero()], &txhash.0).unwrap(),
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }
}
