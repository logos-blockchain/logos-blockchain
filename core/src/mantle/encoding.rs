use lb_groth16::{CompressedGroth16Proof, Fr, fr_from_bytes};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkPublicKey, ZkSignature};
use nom::{
    IResult, Parser as _,
    bytes::complete::take,
    combinator::{map, map_res},
    error::{Error, ErrorKind},
    multi::{count, length_count},
    number::complete::{le_u16, le_u32, le_u64, u8 as decode_u8},
    sequence::pair,
};
use time::OffsetDateTime;

use crate::{
    mantle::{
        MantleTx, Note, NoteId, SignedMantleTx,
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, Ed25519PublicKey, MsgId, deposit::DepositOp, inscribe::InscriptionOp,
                set_keys::SetKeysOp,
            },
            leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier},
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
    },
    proofs::leader_claim_proof::Groth16LeaderClaimProof,
    sdp::{ActivityMetadata, DeclarationId, Locator, ProviderId, ServiceType},
};

// ==============================================================================
// Memory Safety Limits
// ==============================================================================
// These limits are not designed to mimic system limits, but rather to prevent
// unbounded memory usage from malicious inputs. They prevent memory
// over-allocation attacks where untrusted input specifies allocation sizes.
// Values are chosen to not limit normal operations while preventing excessive
// memory usage (e.g., 68GB allocation). As an example, if the network currently
// limits maximum transaction size to 1MiB, for memory safety limits we can
// allow 4MiB.

/// Maximum memory allocation size allowed for channel inscription data .
/// Protects against unbounded allocation in `decode_channel_inscribe`
pub const MAX_ENCODE_DECODE_INSCRIPTION_SIZE: u32 = (MAX_BLOCK_SIZE * 7 / 8) as u32;
// Maximum memory allocation size allowed for SDP activity metadata.
// Protects against unbounded allocation in `decode_sdp_active`
const MAX_ENCODE_DECODE_METADATA_SIZE: u32 = 234; // `ActiveMessage` has a fixed size of 234 bytes

// Maximum byte size allowed for a locator in SDPDeclare operations.
const LOCATOR_BYTES_SIZE_LIMIT: usize = 329usize;

// ==============================================================================
// Top-Level Transaction Decoders
// ==============================================================================

pub fn decode_signed_mantle_tx(input: &[u8]) -> IResult<&[u8], SignedMantleTx> {
    // SignedMantleTx = MantleTx OpsProofs
    let (input, mantle_tx) = decode_mantle_tx(input)?;
    let (input, ops_proofs) = decode_ops_proofs(input, mantle_tx.ops())?;

    let signed_tx = SignedMantleTx::new(mantle_tx, ops_proofs)
        .map_err(|_| nom::Err::Error(Error::new(input, ErrorKind::Verify)))?;

    Ok((input, signed_tx))
}

pub fn decode_mantle_tx(input: &[u8]) -> IResult<&[u8], MantleTx> {
    // MantleTx = Ops ExecutionGasPrice StorageGasPrice
    let (input, ops) = decode_ops(input)?;

    Ok((input, ops.into()))
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
        opcode::SET_CHANNEL_KEYS => map(decode_channel_set_keys, Op::ChannelSetKeys).parse(input),
        opcode::CHANNEL_DEPOSIT => map(decode_channel_deposit, Op::ChannelDeposit).parse(input),
        opcode::CHANNEL_WITHDRAW => map(decode_channel_withdraw, Op::ChannelWithdraw).parse(input),
        opcode::SDP_DECLARE => map(decode_sdp_declare, Op::SDPDeclare).parse(input),
        opcode::SDP_WITHDRAW => map(decode_sdp_withdraw, Op::SDPWithdraw).parse(input),
        opcode::SDP_ACTIVE => map(decode_sdp_active, Op::SDPActive).parse(input),
        opcode::LEADER_CLAIM => map(decode_leader_claim, Op::LeaderClaim).parse(input),
        opcode::TRANSFER => map(decode_transfer, Op::Transfer).parse(input),
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

    // Validate inscription length to prevent unbounded memory allocation
    if inscription_len > MAX_ENCODE_DECODE_INSCRIPTION_SIZE {
        return Err(nom::Err::Error(Error::new(input, ErrorKind::TooLarge)));
    }

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

fn decode_channel_set_keys(input: &[u8]) -> IResult<&[u8], SetKeysOp> {
    // ChannelSetKeys = ChannelId KeyCount *Ed25519PublicKey
    let (input, channel) = map(decode_hash32, ChannelId::from).parse(input)?;
    let (input, key_count) = decode_byte(input)?;

    let (input, keys) = count(decode_ed25519_public_key, key_count as usize).parse(input)?;

    Ok((input, SetKeysOp { channel, keys }))
}

fn decode_channel_deposit(input: &[u8]) -> IResult<&[u8], DepositOp> {
    // ChannelDeposit = ChannelId Amount Metadata
    let (input, channel_id) = map(decode_hash32, ChannelId::from).parse(input)?;
    let (input, inputs) = decode_inputs(input)?;
    let (input, metadata_len) = decode_uint32(input)?;
    let (input, metadata) =
        map(take(metadata_len as usize), |bytes: &[u8]| bytes.to_vec()).parse(input)?;

    Ok((
        input,
        DepositOp {
            channel_id,
            inputs,
            metadata,
        },
    ))
}

fn decode_channel_withdraw(input: &[u8]) -> IResult<&[u8], ChannelWithdrawOp> {
    // ChannelWithdraw = ChannelId Amount
    let (input, channel_id) = map(decode_hash32, ChannelId::from).parse(input)?;
    let (input, outputs) = decode_outputs(input)?;
    let (input, withdraw_nonce) = decode_uint32(input)?;
    Ok((
        input,
        ChannelWithdrawOp {
            channel_id,
            outputs,
            withdraw_nonce,
        },
    ))
}

// ==============================================================================
// SDP Operation Decoders
// ==============================================================================

fn decode_sdp_declare(input: &[u8]) -> IResult<&[u8], SDPDeclareOp> {
    // SDPDeclare = ServiceType LocatorCount *Locator ProviderId ZkId LockedNoteId
    let (input, service_type_byte) = decode_byte(input)?;
    let service_type = match service_type_byte {
        0 => ServiceType::BlendNetwork,
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

    // Validate metadata length to prevent unbounded memory allocation
    if metadata_len > MAX_ENCODE_DECODE_METADATA_SIZE {
        return Err(nom::Err::Error(Error::new(input, ErrorKind::TooLarge)));
    }

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
    let (input, pk) = decode_zk_public_key(input)?;

    Ok((
        input,
        LeaderClaimOp {
            rewards_root: RewardsRoot::from(rewards_root_fr),
            voucher_nullifier: VoucherNullifier::from(voucher_nullifier_fr),
            pk,
        },
    ))
}

// ==============================================================================
// Transfer Decoders
// ==============================================================================

fn decode_note(input: &[u8]) -> IResult<&[u8], Note> {
    // Note = Value ZkPublicKey
    let (input, value) = decode_uint64(input)?;
    let (input, pk) = decode_zk_public_key(input)?;

    Ok((input, Note::new(value, pk)))
}

fn decode_inputs(input: &[u8]) -> IResult<&[u8], Inputs> {
    // Inputs = InputCount *NoteId
    let (input, input_count) = decode_byte(input)?;

    let (input, note_ids) =
        count(map(decode_field_element, NoteId), input_count as usize).parse(input)?;
    Ok((input, Inputs::new(note_ids)))
}

fn decode_outputs(input: &[u8]) -> IResult<&[u8], Outputs> {
    // Outputs = OutputCount *Note
    let (input, output_count) = decode_byte(input)?;
    let (input, notes) = count(decode_note, output_count as usize).parse(input)?;

    Ok((input, Outputs::new(notes)))
}

fn decode_transfer(input: &[u8]) -> IResult<&[u8], TransferOp> {
    // Transfer = Inputs Outputs
    let (input, inputs) = decode_inputs(input)?;
    let (input, outputs) = decode_outputs(input)?;

    Ok((input, TransferOp::new(inputs, outputs)))
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
        Op::ChannelInscribe(_) | Op::ChannelSetKeys(_) => {
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
        Op::SDPWithdraw(_) | Op::SDPActive(_) | Op::Transfer(_) | Op::ChannelDeposit(_) => {
            map(decode_zk_signature, OpProof::ZkSig).parse(input)
        }

        // ProofOfClaimProof = Groth16
        Op::LeaderClaim(leader_claim_op) => map(decode_groth16, |proof| {
            OpProof::PoC(Groth16LeaderClaimProof::new(
                proof,
                leader_claim_op.voucher_nullifier,
            ))
        })
        .parse(input),

        // ChannelWithdrawProof
        Op::ChannelWithdraw(_) => {
            map(decode_channel_withdraw_proof, OpProof::ChannelWithdrawProof).parse(input)
        }
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

const fn calculate_channel_withdraw_proof_byte_size(
    channel_withdraw_threshold: ChannelKeyIndex,
) -> usize {
    (channel_withdraw_threshold as usize) * (ED25519_SIG_BYTES + 4)
}

fn decode_channel_withdraw_proof(input: &[u8]) -> IResult<&[u8], ChannelWithdrawProof> {
    // ChannelWithdrawProof = SignatureCount *WithdrawSignature
    // WithdrawSignature = Ed25519Signature Index
    let (input, signatures) = length_count(
        map(decode_uint16, |n: ChannelKeyIndex| n as usize),
        pair(decode_ed25519_signature, decode_uint16),
    )
    .parse(input)?;

    let signatures: Vec<WithdrawSignature> = signatures
        .into_iter()
        .map(|(signature, index)| WithdrawSignature::from((index, signature)))
        .collect();

    ChannelWithdrawProof::new(signatures)
        .map(|proof| (input, proof))
        .map_err(|_| nom::Err::Failure(Error::new(input, ErrorKind::Verify)))
}

pub(crate) fn decode_field_element(input: &[u8]) -> IResult<&[u8], Fr> {
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

pub(crate) fn decode_utf8_string(input: &[u8], len: usize) -> IResult<&[u8], String> {
    map_res(take(len), |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .map(ToOwned::to_owned)
            .map_err(|_| Error::new(bytes, ErrorKind::Fail))
    })
    .parse(input)
}

fn decode_uint16(input: &[u8]) -> IResult<&[u8], u16> {
    // UINT16 = 2BYTE
    le_u16(input)
}

fn decode_uint32(input: &[u8]) -> IResult<&[u8], u32> {
    // UINT32 = 4BYTE
    le_u32(input)
}

pub(crate) fn decode_uint64(input: &[u8]) -> IResult<&[u8], u64> {
    // UINT64 = 8BYTE
    le_u64(input)
}

fn decode_byte(input: &[u8]) -> IResult<&[u8], u8> {
    // Byte = OCTET
    decode_u8(input)
}

pub(crate) fn decode_unix_timestamp(input: &[u8]) -> IResult<&[u8], OffsetDateTime> {
    // Timestamp = UINT64
    map_res(decode_uint64, |ts| {
        OffsetDateTime::from_unix_timestamp(
            ts.try_into()
                .map_err(|_| Error::new(input, ErrorKind::Fail))?,
        )
        .map_err(|_| Error::new(input, ErrorKind::Fail))
    })
    .parse(input)
}

// ==============================================================================
// Binary Encoders
// ==============================================================================

use lb_groth16::fr_to_bytes;

use super::ops::opcode;
use crate::{
    block::MAX_BLOCK_SIZE,
    mantle::{
        ledger::{Inputs, Outputs},
        ops::channel::{ChannelKeyIndex, withdraw::ChannelWithdrawOp},
        tx::MantleTxGasContext,
    },
    proofs::channel_withdraw_proof::{ChannelWithdrawProof, WithdrawSignature},
};
// Encode primitives

/// Encode primitives
fn encode_uint16(value: u16) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn encode_uint32(value: u32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

pub(crate) fn encode_uint64(value: u64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn encode_byte(value: u8) -> Vec<u8> {
    vec![value]
}

pub(crate) fn encode_string(s: &String) -> Vec<u8> {
    s.as_bytes().to_vec()
}

pub(crate) fn encode_unix_timestamp(ts: &OffsetDateTime) -> Vec<u8> {
    encode_uint64(
        ts.unix_timestamp()
            .try_into()
            .expect("timestamp fits in u64"),
    )
}

fn encode_hash32(hash: &[u8; 32]) -> Vec<u8> {
    hash.to_vec()
}

pub(crate) fn encode_field_element(fr: &Fr) -> Vec<u8> {
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
    encode_groth16_proof(sig.as_proof())
}

fn encode_poc(poc: &Groth16LeaderClaimProof) -> Vec<u8> {
    // Groth16LeaderClaimProof wraps PocProof which is CompressedGroth16Proof
    encode_groth16_proof(poc.proof())
}

fn encode_groth16_proof(proof: &CompressedGroth16Proof) -> Vec<u8> {
    proof.to_bytes().to_vec()
}

fn encode_channel_withdraw_proof(proof: &ChannelWithdrawProof) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_uint16(proof.signatures().len() as ChannelKeyIndex));
    bytes.extend(proof.signatures().iter().flat_map(|signature| {
        encode_ed25519_signature(&signature.signature)
            .into_iter()
            .chain(encode_uint16(signature.channel_key_index))
    }));
    bytes
}

/// Encode channel operations
#[must_use]
pub fn encode_channel_inscribe(op: &InscriptionOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(op.channel_id.as_ref()));
    assert!(
        op.inscription.len() <= MAX_ENCODE_DECODE_INSCRIPTION_SIZE as usize,
        "Fatal error in 'encode_channel_inscribe' - {} inscription data clipped to {}",
        op.inscription.len(),
        MAX_ENCODE_DECODE_INSCRIPTION_SIZE
    );
    bytes.extend(encode_uint32(op.inscription.len() as u32));
    bytes.extend(&op.inscription);
    bytes.extend(encode_hash32(op.parent.as_ref()));
    bytes.extend(encode_ed25519_public_key(&op.signer));
    bytes
}

fn encode_channel_set_keys(op: &SetKeysOp) -> Vec<u8> {
    assert!(
        u8::try_from(op.keys.len()).is_ok(),
        "Fatal error in 'encode_channel_set_keys' - {} keys clipped to {}",
        op.keys.len(),
        u8::MAX
    );
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(op.channel.as_ref()));
    bytes.extend(encode_byte(op.keys.len() as u8));
    for key in &op.keys {
        bytes.extend(encode_ed25519_public_key(key));
    }
    bytes
}

fn encode_channel_deposit(op: &DepositOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(op.channel_id.as_ref()));
    bytes.extend(encode_inputs(op.inputs.as_ref()));
    bytes.extend(encode_uint32(op.metadata.len() as u32));
    bytes.extend(op.metadata.as_slice());
    bytes
}

#[must_use]
pub fn encode_channel_withdraw(op: &ChannelWithdrawOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_hash32(op.channel_id.as_ref()));
    bytes.extend(encode_outputs(op.outputs.as_ref()));
    bytes.extend(encode_uint32(op.withdraw_nonce));
    bytes
}

/// Encode SDP operations
fn encode_locator(locator: &multiaddr::Multiaddr) -> Vec<u8> {
    let locator_bytes = locator.to_vec();
    assert!(
        locator_bytes.len() <= LOCATOR_BYTES_SIZE_LIMIT,
        "Fatal error in 'encode_locator' - {} locator bytes clipped to \
            {LOCATOR_BYTES_SIZE_LIMIT}",
        locator_bytes.len()
    );
    let mut bytes = Vec::new();
    bytes.extend((locator_bytes.len() as u16).to_le_bytes());
    bytes.extend(locator_bytes);
    bytes
}

fn encode_sdp_declare(op: &SDPDeclareOp) -> Vec<u8> {
    assert!(
        u8::try_from(op.locators.len()).is_ok(),
        "Fatal error in 'encode_sdp_declare' - {} locators clipped to {}",
        op.locators.len(),
        u8::MAX
    );
    let mut bytes = Vec::new();
    // ServiceType
    let service_type_byte = match op.service_type {
        ServiceType::BlendNetwork => 0u8,
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
    assert!(
        metadata_bytes.len() <= MAX_ENCODE_DECODE_METADATA_SIZE as usize,
        "Fatal error in 'encode_sdp_active' - {} metadata bytes clipped to {}",
        metadata_bytes.len(),
        MAX_ENCODE_DECODE_METADATA_SIZE
    );

    bytes.extend(encode_uint32(metadata_bytes.len() as u32));
    bytes.extend(&metadata_bytes);
    bytes
}

/// Encode leader operations
#[must_use]
pub fn encode_leader_claim(op: &LeaderClaimOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field_element(&op.rewards_root.into()));
    bytes.extend(encode_field_element(&op.voucher_nullifier.into()));
    bytes.extend(encode_field_element(op.pk.as_fr()));
    bytes
}

/// Encode transfer operation
fn encode_note(note: &Note) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_uint64(note.value));
    bytes.extend(encode_field_element(note.pk.as_fr()));
    bytes
}

fn encode_inputs(inputs: &[NoteId]) -> Vec<u8> {
    assert!(
        u8::try_from(inputs.len()).is_ok(),
        "Fatal error in 'encode_inputs' - {} inputs clipped to {}",
        inputs.len(),
        u8::MAX
    );
    let mut bytes = Vec::new();
    bytes.extend(encode_byte(inputs.len() as u8));
    for input in inputs {
        bytes.extend(encode_field_element(input.as_ref()));
    }
    bytes
}

fn encode_outputs(outputs: &[Note]) -> Vec<u8> {
    let mut bytes = Vec::new();
    assert!(
        u8::try_from(outputs.len()).is_ok(),
        "Fatal error in 'encode_outputs' - {} outputs clipped to {}",
        outputs.len(),
        u8::MAX
    );
    bytes.extend(encode_byte(outputs.len() as u8));
    for output in outputs {
        bytes.extend(encode_note(output));
    }
    bytes
}

#[must_use]
pub fn encode_transfer_op(op: &TransferOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_inputs(op.inputs.as_ref()));
    bytes.extend(encode_outputs(op.outputs.as_ref()));
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
        Op::ChannelSetKeys(op) => {
            bytes.extend(encode_byte(opcode::SET_CHANNEL_KEYS));
            bytes.extend(encode_channel_set_keys(op));
        }
        Op::ChannelDeposit(op) => {
            bytes.extend(encode_byte(opcode::CHANNEL_DEPOSIT));
            bytes.extend(encode_channel_deposit(op));
        }
        Op::ChannelWithdraw(op) => {
            bytes.extend(encode_byte(opcode::CHANNEL_WITHDRAW));
            bytes.extend(encode_channel_withdraw(op));
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
        Op::Transfer(op) => {
            bytes.extend(encode_byte(opcode::TRANSFER));
            bytes.extend(encode_transfer_op(op));
        }
    }
    bytes
}

fn encode_ops(ops: &[Op]) -> Vec<u8> {
    assert!(
        u8::try_from(ops.len()).is_ok(),
        "Fatal error in 'encode_ops' - {} ops clipped to {}",
        ops.len(),
        u8::MAX
    );
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
        (OpProof::Ed25519Sig(sig), Op::ChannelInscribe(_) | Op::ChannelSetKeys(_)) => {
            encode_ed25519_signature(sig)
        }
        (OpProof::ChannelWithdrawProof(proof), Op::ChannelWithdraw(_)) => {
            encode_channel_withdraw_proof(proof)
        }
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
        (
            OpProof::ZkSig(sig),
            Op::SDPWithdraw(_) | Op::SDPActive(_) | Op::Transfer(_) | Op::ChannelDeposit(_),
        ) => encode_zk_signature(sig),
        (OpProof::PoC(poc), Op::LeaderClaim(_)) => encode_poc(poc),
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
    encode_ops(tx.ops())
}

#[must_use]
pub fn encode_signed_mantle_tx(tx: &SignedMantleTx) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_mantle_tx(&tx.mantle_tx));
    bytes.extend(encode_ops_proofs(&tx.ops_proofs, tx.mantle_tx.ops()));
    bytes
}

pub(crate) fn predict_signed_mantle_tx_size(tx: &MantleTx, context: &MantleTxGasContext) -> usize {
    let mantle_tx_size = encode_mantle_tx(tx).len();

    let ops_proofs_size = tx
        .ops()
        .iter()
        .map(|op| match op {
            // Ed25519SigProof = Ed25519Signature
            Op::ChannelInscribe(_) | Op::ChannelSetKeys(_) => ED25519_SIG_BYTES,

            // ZkAndEd25519SigsProof = ZkSignature Ed25519Signature
            Op::SDPDeclare(_) => GROTH16_BYTES + ED25519_SIG_BYTES,

            // ZkSigProof = ZkSignature = ProofOfClaimProof = Groth16
            Op::SDPWithdraw(_) | Op::SDPActive(_) | Op::LeaderClaim(_) | Op::Transfer(_) => {
                GROTH16_BYTES
            }

            // WithdrawProof
            Op::ChannelWithdraw(operation) => {
                let channel_withdraw_threshold = context.withdraw_threshold(&operation.channel_id).expect(
                    "Operation should have been verified before reaching this point, so the channel must exist in the context."
                );
                calculate_channel_withdraw_proof_byte_size(channel_withdraw_threshold)
            }

            // None
            Op::ChannelDeposit(_) => 0,
        })
        .sum::<usize>();

    mantle_tx_size + ops_proofs_size
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, panic};

    use ark_ff::Field as _;
    use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{Transaction as _, tx::GasPrices},
        sdp::blend::ActivityProof,
    };

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

    #[test]
    fn test_encode_decode_primitives() {
        // Test UINT64
        let data = encode_uint64(42u64);
        let (remaining, value) = decode_uint64(&data).unwrap();
        assert_eq!(value, 42u64);
        assert!(remaining.is_empty());

        // Test UINT32
        let data = encode_uint32(123u32);
        let (remaining, value) = decode_uint32(&data).unwrap();
        assert_eq!(value, 123u32);
        assert!(remaining.is_empty());

        // Test Byte
        let data = encode_byte(0xAB);
        let (remaining, value) = decode_byte(&data).unwrap();
        assert_eq!(value, 0xAB);
        assert!(remaining.is_empty());

        // Test Hash32
        let data = encode_hash32(&[0x42u8; 32]);
        let (remaining, value) = decode_hash32(&data).unwrap();
        assert_eq!(value, [0x42u8; 32]);
        assert!(remaining.is_empty());

        // Test UTF-8 String
        let str = "hello, world!".to_owned();
        let data = encode_string(&str);
        let (remaining, value) = decode_utf8_string(&data, data.len()).unwrap();
        assert_eq!(value, str);
        assert!(remaining.is_empty());

        // Test Unix Timestamp
        let ts = OffsetDateTime::now_utc();
        let data = encode_unix_timestamp(&ts);
        let (remaining, value) = decode_unix_timestamp(&data).unwrap();
        assert_eq!(value, ts.truncate_to_second());
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_decode_signed_mantle_tx_empty() {
        let mantle_tx = MantleTx(vec![]);

        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![],
        };

        #[expect(
            clippy::string_add,
            reason = "Recommended String::push_str does not support chaining"
        )]
        let test_vector = String::new() + "00"; // OpCount=0u8

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
        let mantle_tx = MantleTx(vec![Op::ChannelInscribe(InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"hello".to_vec(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        })]);

        let txhash = mantle_tx.hash();
        let inscribe_sig =
            OpProof::Ed25519Sig(signing_key.sign_payload(&txhash.as_signing_bytes()));
        let signed_tx = SignedMantleTx::new(mantle_tx, vec![inscribe_sig]).unwrap();

        #[expect(
            clippy::string_add,
            reason = "Recommended String::push_str does not support chaining"
        )]
        let test_vector = String::new()
            + "01"                                                               // OpCount
            + "11"                                                               // OpCode
            + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" // ChannelID (32Byte)
            + "05000000"                                                         // InscriptionLength
            + "68656c6c6f"                                                       // Inscription
            + "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" // Parent (32Byte)
            + "ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c" // Signer (32Byte)
            + "4ec789fc67b7f7bfba02f8cc7f3f671a107225faefbe60ca0b8e9e7e8e43e8db" // Signature (64Byte)
            + "835075aed539fac37e0fdc03acc2aba873e43eef8a835476c4c6bdaaba866901";

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
        let mantle_tx = MantleTx(vec![
            Op::ChannelInscribe(InscriptionOp {
                channel_id: ChannelId::from([0x11; 32]),
                inscription: b"first".to_vec(),
                parent: MsgId::from([0x00; 32]),
                signer: signing_key.public_key(),
            }),
            Op::ChannelSetKeys(SetKeysOp {
                channel: ChannelId::from([0x22; 32]),
                keys: vec![signing_key.public_key()],
            }),
        ]);

        let txhash = mantle_tx.hash();
        let sig = signing_key.sign_payload(&txhash.as_signing_bytes());

        // Encode and decode roundtrip test (no hardcoded test vector since signatures
        // are deterministic)
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(sig), OpProof::Ed25519Sig(sig)],
        )
        .unwrap();

        let encoded = encode_signed_mantle_tx(&signed_tx);
        let (remaining, decoded_tx) = decode_signed_mantle_tx(&encoded).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(decoded_tx, signed_tx);
    }

    #[tokio::test]
    async fn test_large_payload_encoding_decoding() {
        // Test payload sizes from 512kB up to 2MiB in 512kB increments
        const MAX_SIZE: usize = MAX_ENCODE_DECODE_INSCRIPTION_SIZE as usize;
        const CHUNK_SIZE: usize = MAX_SIZE / 10;

        let signing_key = Ed25519Key::from_bytes(&[1; 32]);

        let mut tasks = Vec::new();

        for payload_size in (CHUNK_SIZE..=MAX_SIZE).step_by(CHUNK_SIZE) {
            let signing_key = signing_key.clone();

            let task = tokio::task::spawn(async move {
                let large_inscription = vec![0xAB; payload_size];

                let inscribe_op = InscriptionOp {
                    channel_id: ChannelId::from([0xAA; 32]),
                    inscription: large_inscription,
                    parent: MsgId::from([0xBB; 32]),
                    signer: signing_key.public_key(),
                };

                let mantle_tx = MantleTx(vec![Op::ChannelInscribe(inscribe_op)]);

                let txhash = mantle_tx.hash();
                let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
                let signed_tx =
                    SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(op_sig)]).unwrap();

                let encoded = encode_signed_mantle_tx(&signed_tx);

                let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
                let predicted_size =
                    predict_signed_mantle_tx_size(&signed_tx.mantle_tx, &gas_context);
                assert_eq!(
                    predicted_size,
                    encoded.len(),
                    "Size mismatch at payload size {payload_size}",
                );

                let (remaining, decoded_tx) = decode_signed_mantle_tx(&encoded)
                    .unwrap_or_else(|_| panic!("Failed to decode at payload size {payload_size}"));

                assert!(
                    remaining.is_empty(),
                    "Unexpected remaining bytes at payload size {payload_size}",
                );
                assert_eq!(
                    decoded_tx, signed_tx,
                    "Roundtrip mismatch at payload size {payload_size}",
                );
            });

            tasks.push(task);
        }

        // Wait for all tasks to complete
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[test]
    fn test_encode_decode_roundtrip_empty_tx() {
        // Create an empty MantleTx
        let original_tx = MantleTx(vec![]);

        // Encode
        let encoded = encode_mantle_tx(&original_tx);

        // Decode
        let (remaining, decoded_tx) = decode_mantle_tx(&encoded).unwrap();

        // Verify
        assert!(remaining.is_empty());
        assert_eq!(original_tx, decoded_tx);
    }

    #[test]
    fn test_encode_decode_roundtrip_with_transfer() {
        use num_bigint::BigUint;

        // Create a MantleTx with ledger inputs and outputs
        let pk = ZkPublicKey::from(BigUint::from(42u64));
        let note = Note::new(1000, pk);
        let note_id = NoteId(BigUint::from(123u64).into());
        let transfer_op = TransferOp::new(Inputs::new(vec![note_id]), Outputs::new(vec![note]));

        let original_tx = MantleTx(vec![Op::Transfer(transfer_op)]);

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
        let mantle_tx = MantleTx(vec![]);
        let original_tx = SignedMantleTx::new(mantle_tx, vec![]).unwrap();

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
        let mantle_tx = MantleTx(vec![]);

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get actual size
        let signed_tx = SignedMantleTx::new(mantle_tx, vec![]).unwrap();
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

        let mantle_tx = MantleTx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get actual size
        let txhash = mantle_tx.hash();
        let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
        let signed_tx = SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(op_sig)]).unwrap();
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

        let mantle_tx = MantleTx(vec![Op::ChannelSetKeys(set_keys_op)]);

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get actual size
        let dummy_ed25519_sig = Ed25519Signature::from_bytes(&[0; 64]);
        let signed_tx =
            SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(dummy_ed25519_sig)]).unwrap();
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
            op_id: [1u8; 32],
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

        let mantle_tx = MantleTx(vec![Op::SDPDeclare(sdp_declare_op)]);

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get actual size
        let txhash = mantle_tx.hash();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkAndEd25519Sigs {
                zk_sig: ZkKey::multi_sign(&[locked_note_sk, zk_sk], &txhash.to_fr()).unwrap(),
                ed25519_sig: Ed25519Signature::from_bytes(&[0u8; 64]),
            }],
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

        let mantle_tx = MantleTx(vec![Op::SDPWithdraw(sdp_withdraw_op)]);

        let txhash = mantle_tx.hash();

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get actual size
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkSig(
                ZkKey::multi_sign(&[ZkKey::zero()], &txhash.to_fr()).unwrap(),
            )],
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_sdp_active() {
        use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};

        let signing_key = Ed25519Key::from_bytes(&[1u8; 32]);
        let blend_proof = ActivityProof {
            session: 42,
            signing_key: signing_key.public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0u8; 160]).into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0u8; 32]).into(),
        };

        let metadata = ActivityMetadata::Blend(Box::new(blend_proof));

        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x22; 32]),
            nonce: 99,
            metadata,
        };

        let mantle_tx = MantleTx(vec![Op::SDPActive(sdp_active_op)]);

        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        let txhash = mantle_tx.hash();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkSig(
                ZkKey::multi_sign(&[ZkKey::zero()], &txhash.to_fr()).unwrap(),
            )],
        )
        .unwrap();

        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_multiple_ops() {
        use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};

        let signing_key = Ed25519Key::from_bytes(&[1; 32]);

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"test".to_vec(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        };

        let set_keys_op = SetKeysOp {
            channel: ChannelId::from([0xCC; 32]),
            keys: vec![signing_key.public_key()],
        };

        let blend_proof = ActivityProof {
            session: u64::MAX,
            signing_key: signing_key.public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0u8; 160]).into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0u8; 32]).into(),
        };

        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x33; 32]),
            nonce: 55,
            metadata: ActivityMetadata::Blend(Box::new(blend_proof)),
        };

        let mantle_tx = MantleTx(vec![
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelSetKeys(set_keys_op),
            Op::SDPActive(sdp_active_op),
        ]);

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        let txhash = mantle_tx.hash();
        let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
        // Create a signed tx and encode it to get actual size
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(op_sig),
                OpProof::Ed25519Sig(op_sig),
                OpProof::ZkSig(ZkKey::zero().sign_payload(&txhash.to_fr()).unwrap()),
            ],
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

        let transfer_op = TransferOp::new(
            Inputs::new(vec![note_id1, note_id2, note_id3]),
            Outputs::new(vec![note1, note2]),
        );

        let mantle_tx = MantleTx(vec![Op::Transfer(transfer_op)]);

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get actual size
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkSig(ZkKey::multi_sign(&[], &Fr::ZERO).unwrap())],
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
        let transfer_op = TransferOp {
            inputs: Inputs::new(vec![NoteId(BigUint::from(777u64).into())]),
            outputs: Outputs::new(vec![Note::new(5000, locked_note_sk.to_public_key())]),
        };

        let locator: multiaddr::Multiaddr = "/dns4/example.com/tcp/443".parse().unwrap();
        let zk_sk = ZkKey::zero();
        let sdp_declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec![Locator::new(locator)],
            provider_id: ProviderId(signing_key1.public_key()),
            zk_id: zk_sk.to_public_key(),
            locked_note_id: transfer_op
                .outputs
                .utxo_by_index(0, &transfer_op)
                .unwrap()
                .id(),
        };

        let mantle_tx = MantleTx(vec![
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelSetKeys(set_keys_op),
            Op::SDPDeclare(sdp_declare_op),
            Op::Transfer(transfer_op),
        ]);

        // Predict size
        let gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get actual size
        let txhash = mantle_tx.hash();
        let op_ed25519_sig = signing_key1.sign_payload(&txhash.as_signing_bytes());
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(op_ed25519_sig),
                OpProof::Ed25519Sig(op_ed25519_sig),
                OpProof::ZkAndEd25519Sigs {
                    zk_sig: ZkKey::multi_sign(&[locked_note_sk, zk_sk], &txhash.to_fr()).unwrap(),
                    ed25519_sig: op_ed25519_sig,
                },
                OpProof::ZkSig(ZkKey::multi_sign(&[], &Fr::ZERO).unwrap()),
            ],
        )
        .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_leader_claim() {
        use crate::proofs::leader_claim_proof::Groth16LeaderClaimProof;

        let leader_claim_op = LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: VoucherNullifier::default(),
            pk: ZkPublicKey::from(BigUint::from(0u64)),
        };

        let mantle_tx = MantleTx(vec![Op::LeaderClaim(leader_claim_op.clone())]);

        let empty_gas_context = MantleTxGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &empty_gas_context);

        let poc_proof = Groth16LeaderClaimProof::new(
            CompressedGroth16Proof::from_bytes(&[0u8; 128]),
            leader_claim_op.voucher_nullifier,
        );

        // Construct directly to skip proof verification (dummy proof won't verify)
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![OpProof::PoC(poc_proof)],
        };

        let encoded = encode_signed_mantle_tx(&signed_tx);
        assert_eq!(predicted_size, encoded.len());
    }

    #[test]
    fn test_encode_decode_leader_claim_op_proof() {
        use crate::proofs::leader_claim_proof::Groth16LeaderClaimProof;

        let proof_bytes: [u8; 128] = core::array::from_fn(|i| i as u8);
        let voucher_nf = VoucherNullifier::default();
        let poc_proof = Groth16LeaderClaimProof::new(
            CompressedGroth16Proof::from_bytes(&proof_bytes),
            voucher_nf,
        );

        let leader_claim_op = LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: voucher_nf,
            pk: ZkPublicKey::from(BigUint::from(0u64)),
        };
        let op = Op::LeaderClaim(leader_claim_op);

        let encoded = encode_op_proof(&OpProof::PoC(poc_proof), &op);
        assert_eq!(encoded.len(), GROTH16_BYTES);

        let (remaining, decoded) = decode_op_proof(&encoded, &op).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(
            decoded,
            OpProof::PoC(Groth16LeaderClaimProof::new(
                CompressedGroth16Proof::from_bytes(&proof_bytes),
                voucher_nf,
            ))
        );
    }

    #[test]
    fn test_encode_decode_leader_claim_op() {
        let leader_claim_op = LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: VoucherNullifier::default(),
            pk: ZkPublicKey::from(BigUint::from(0u64)),
        };
        let op = Op::LeaderClaim(leader_claim_op);

        let encoded = encode_op(&op);
        let (remaining, decoded_op) = decode_op(&encoded).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(decoded_op, op);
    }

    #[test]
    fn test_encode_decode_channel_withdraw_tx() {
        let pk1 = ZkPublicKey::from(BigUint::from(100u64));
        let pk2 = ZkPublicKey::from(BigUint::from(200u64));

        let note1 = Note::new(1000, pk1);
        let note2 = Note::new(2000, pk2);

        let signing_key = Ed25519Key::from_bytes(&[21u8; 32]);
        let mantle_tx = MantleTx(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id: ChannelId::from([0xAB; 32]),
            outputs: Outputs::new(vec![note1, note2]),
            withdraw_nonce: 0,
        })]);
        let tx_hash = mantle_tx.hash();
        let proof = ChannelWithdrawProof::new(vec![WithdrawSignature::new(
            0,
            signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
        )])
        .unwrap();
        let signed_tx =
            SignedMantleTx::new(mantle_tx, vec![OpProof::ChannelWithdrawProof(proof)]).unwrap();

        let encoded = encode_signed_mantle_tx(&signed_tx);
        let (remaining, decoded_tx) = decode_signed_mantle_tx(&encoded).unwrap();

        assert!(remaining.is_empty());
        assert_eq!(decoded_tx, signed_tx);
    }

    // ==============================================================================
    // Security Tests - Memory Over-Allocation Protection
    // ==============================================================================

    #[test]
    fn test_encode_reject_oversized_inscription() {
        let oversized_inscription = vec![0xAB; MAX_ENCODE_DECODE_INSCRIPTION_SIZE as usize + 1];

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: oversized_inscription,
            parent: MsgId::from([0xBB; 32]),
            signer: Ed25519Key::from_bytes(&[1; 32]).public_key(),
        };

        let result = panic::catch_unwind(|| {
            let _unused = encode_channel_inscribe(&inscribe_op);
        });
        assert!(
            result.is_err(),
            "Should reject encoding of oversized inscription"
        );
    }

    #[test]
    fn test_decode_reject_oversized_inscription() {
        // Create a malicious input with inscription_len = MAX_INSCRIPTION_SIZE + 1
        let mut malicious_input = Vec::new();

        // ChannelId (32 bytes)
        malicious_input.extend_from_slice(&[0x42; 32]);

        // Inscription length (u32) - exceeds MAX_INSCRIPTION_SIZE
        let oversized_len = MAX_ENCODE_DECODE_INSCRIPTION_SIZE + 1;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // We don't need to include the actual inscription data because
        // the decoder should reject it before trying to read that much

        // Try to decode - should fail with TooLarge error
        let result = decode_channel_inscribe(&malicious_input);
        assert!(result.is_err(), "Should reject oversized inscription");

        // Verify it fails with the right error kind
        match result {
            Err(nom::Err::Error(e)) => {
                assert_eq!(e.code, ErrorKind::TooLarge);
            }
            _ => panic!("Expected TooLarge error"),
        }
    }

    #[test]
    fn test_decode_reject_oversized_metadata() {
        // Create a malicious input with metadata_len = MAX_METADATA_SIZE + 1
        let mut malicious_input = Vec::new();

        // DeclarationId (32 bytes)
        malicious_input.extend_from_slice(&[0x42; 32]);

        // Nonce (u64)
        malicious_input.extend_from_slice(&42u64.to_le_bytes());

        // Metadata length (u32) - exceeds MAX_METADATA_SIZE
        let oversized_len = MAX_ENCODE_DECODE_METADATA_SIZE + 1;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // Try to decode - should fail with TooLarge error
        let result = decode_sdp_active(&malicious_input);
        assert!(result.is_err(), "Should reject oversized metadata");

        // Verify it fails with the right error kind
        match result {
            Err(nom::Err::Error(e)) => {
                assert_eq!(e.code, ErrorKind::TooLarge);
            }
            _ => panic!("Expected TooLarge error"),
        }
    }

    #[test]
    fn test_encode_reject_excessive_op_count() {
        let ops = vec![
            Op::ChannelSetKeys(SetKeysOp {
                channel: ChannelId::from([0x22; 32]),
                keys: vec![Ed25519Key::from_bytes(&[1; 32]).public_key()],
            });
            u8::MAX as usize + 1
        ];

        let result = panic::catch_unwind(|| {
            encode_ops(&ops);
        });
        assert!(result.is_err(), "Should reject excessive output count");
    }

    #[test]
    fn test_decode_accept_max_op_count() {
        // Test that op_count = MAX_OP_COUNT is accepted
        // (though it will fail later due to missing op data, which is fine for this
        // test)
        let valid_input = vec![u8::MAX];

        // Should not fail with TooLarge error (will fail with incomplete data)
        let result = decode_ops(&valid_input);
        if let Err(nom::Err::Error(e)) = result {
            assert_ne!(e.code, ErrorKind::TooLarge, "Should not reject at u8::MAX]");
        }
    }

    #[test]
    fn test_decode_accept_max_inscription_size() {
        // Test that we can decode an inscription at exactly MAX_INSCRIPTION_SIZE
        let mut valid_input = Vec::new();

        // ChannelId (32 bytes)
        valid_input.extend_from_slice(&[0x42; 32]);

        // Inscription length (u32) - exactly MAX_INSCRIPTION_SIZE
        valid_input.extend_from_slice(&MAX_ENCODE_DECODE_INSCRIPTION_SIZE.to_le_bytes());

        // Inscription data (MAX_INSCRIPTION_SIZE bytes)
        valid_input.extend_from_slice(&vec![0x01; MAX_ENCODE_DECODE_INSCRIPTION_SIZE as usize]);

        // Parent MsgId (32 bytes)
        valid_input.extend_from_slice(&[0x43; 32]);

        // Signer Ed25519PublicKey (32 bytes)
        let sk = Ed25519Key::from_bytes(&[0x44; 32]);
        let pk = sk.public_key();
        valid_input.extend_from_slice(&pk.to_bytes());

        // Should succeed (though signature validation might fail later)
        let result = decode_channel_inscribe(&valid_input);
        assert!(
            result.is_ok(),
            "Should accept inscription at MAX_INSCRIPTION_SIZE: {result:?}",
        );

        let (_, inscription_op) = result.unwrap();
        assert_eq!(
            inscription_op.inscription.len(),
            MAX_ENCODE_DECODE_INSCRIPTION_SIZE as usize
        );
    }

    #[test]
    fn test_decode_memory_safety_no_allocation_on_oversized_length() {
        // This test verifies that we reject oversized lengths WITHOUT
        // attempting to allocate the memory first

        // Test with an astronomically large inscription_len
        // (e.g., 4GB which would cause the original bug)
        let huge_len = u32::MAX; // 4GB - 1

        let mut malicious_input = Vec::new();
        malicious_input.extend_from_slice(&[0x42; 32]); // ChannelId
        malicious_input.extend_from_slice(&huge_len.to_le_bytes());

        // This should fail immediately without trying to allocate 4GB
        let result = decode_channel_inscribe(&malicious_input);
        assert!(result.is_err(), "Should reject huge inscription length");

        // Similar test for metadata
        let mut malicious_input2 = Vec::new();
        malicious_input2.extend_from_slice(&[0x42; 32]); // DeclarationId
        malicious_input2.extend_from_slice(&42u64.to_le_bytes()); // Nonce
        malicious_input2.extend_from_slice(&huge_len.to_le_bytes());

        let result2 = decode_sdp_active(&malicious_input2);
        assert!(result2.is_err(), "Should reject huge metadata length");
    }

    #[test]
    fn test_encode_reject_excessive_key_count() {
        let set_keys_op = SetKeysOp {
            channel: ChannelId::from([0x22; 32]),
            keys: vec![Ed25519Key::from_bytes(&[1; 32]).public_key(); u8::MAX as usize + 1],
        };

        // Should panic
        let result = panic::catch_unwind(|| {
            encode_channel_set_keys(&set_keys_op);
        });
        assert!(result.is_err(), "Should reject excessive output count");
    }

    #[test]
    fn test_decode_accept_max_key_count() {
        // Test that key_count = MAX_KEY_COUNT is accepted
        let mut valid_input = Vec::new();

        // ChannelId (32 bytes)
        valid_input.extend_from_slice(&[0x42; 32]);

        // KeyCount = MAX_KEY_COUNT
        valid_input.push(u8::MAX);

        // Add MAX_KEY_COUNT Ed25519 public keys (each 32 bytes)
        for i in 0..u8::MAX {
            let sk = Ed25519Key::from_bytes(&[i; 32]);
            let pk = sk.public_key();
            valid_input.extend_from_slice(&pk.to_bytes());
        }

        let result = decode_channel_set_keys(&valid_input);
        assert!(result.is_ok(), "Should accept max key count: {result:?}");

        let (_, set_keys_op) = result.unwrap();
        assert_eq!(set_keys_op.keys.len(), u8::MAX as usize);
    }

    #[test]
    fn test_encode_reject_excessive_sdp_declare() {
        let locator: multiaddr::Multiaddr = "/dns4/example.com/tcp/443".parse().unwrap();
        let sdp_declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec![Locator::new(locator); u8::MAX as usize + 1], // excessive locator count
            provider_id: ProviderId(Ed25519Key::from_bytes(&[1; 32]).public_key()),
            zk_id: ZkKey::zero().to_public_key(),
            locked_note_id: NoteId(BigUint::from(111u64).into()),
        };

        // Should panic
        let result = panic::catch_unwind(|| {
            encode_sdp_declare(&sdp_declare_op);
        });
        assert!(result.is_err(), "Should reject excessive output count");
    }

    #[test]
    fn test_encode_reject_excessive_sdp_active() {
        let blend_proof = ActivityProof {
            session: u64::MAX,
            signing_key: Ed25519Key::from_bytes(&[1; 32]).public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0u8; 160]).into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0u8; 32]).into(),
        };
        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x33; 32]),
            nonce: u64::MAX,
            metadata: ActivityMetadata::Blend(Box::new(blend_proof)),
        };
        assert_eq!(
            sdp_active_op.metadata.to_metadata_bytes().len(),
            MAX_ENCODE_DECODE_METADATA_SIZE as usize,
            "`ActiveMessage` has a fixed size of 234 bytes"
        );
    }

    #[test]
    fn test_encode_reject_excessive_input_count() {
        let note_id = NoteId(BigUint::from(111u64).into());
        let inputs = [note_id; u8::MAX as usize + 1];

        // Should panic
        let result = panic::catch_unwind(|| {
            encode_inputs(&inputs);
        });
        assert!(result.is_err(), "Should reject excessive output count");
    }

    #[test]
    fn test_encode_reject_excessive_output_count() {
        let note = Note::new(1000, ZkPublicKey::from(BigUint::from(42u64)));
        let outputs = [note; u8::MAX as usize + 1];

        // Should panic
        let result = panic::catch_unwind(|| {
            encode_outputs(&outputs);
        });
        assert!(result.is_err(), "Should reject excessive output count");
    }

    #[test]
    fn test_decode_reject_oversized_locator() {
        // Create a malicious input with oversized locator
        let mut malicious_input = Vec::new();

        // ServiceType (1 byte)
        malicious_input.push(0x00);

        // LocatorCount (1 byte) - just 1 locator
        malicious_input.push(1);

        let oversized_len = (LOCATOR_BYTES_SIZE_LIMIT + 1) as u16;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // Add the oversized data
        malicious_input.extend_from_slice(&vec![0x01; LOCATOR_BYTES_SIZE_LIMIT + 1]);

        // ... rest of SDPDeclare fields ...

        let result = decode_sdp_declare(&malicious_input);
        if let Err(nom::Err::Error(ref e)) = result {
            assert_eq!(
                e.code,
                ErrorKind::LengthValue,
                "Should reject at `LOCATOR_BYTES_SIZE_LIMIT + 1`"
            );
        } else {
            panic!("Should reject oversized locator");
        }
    }

    #[test]
    fn test_decode_accept_max_locator_size() {
        // Create a malicious input with oversized locator
        let mut malicious_input = Vec::new();

        // ServiceType (1 byte)
        malicious_input.push(0x00);

        // LocatorCount (1 byte) - just 1 locator
        malicious_input.push(1);

        let oversized_len = (LOCATOR_BYTES_SIZE_LIMIT) as u16;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // Add the oversized data
        malicious_input.extend_from_slice(&vec![0x01; LOCATOR_BYTES_SIZE_LIMIT]);

        // ... rest of SDPDeclare fields ...

        let result = decode_sdp_declare(&malicious_input);
        if let Err(nom::Err::Error(ref e)) = result {
            assert_ne!(
                e.code,
                ErrorKind::LengthValue,
                "Should not reject at `LOCATOR_BYTES_SIZE_LIMIT`"
            );
        }
        assert!(result.is_err(), "Should reject invalid declaration");
    }

    #[test]
    fn test_encode_decode_max_inputs() {
        let note_id = NoteId(BigUint::from(111u64).into());
        let inputs = [note_id; u8::MAX as usize];

        // Encode should succeed
        let encoded = encode_inputs(&inputs);
        assert!(
            !encoded.is_empty(),
            "Encoding max input count should produce some output"
        );

        // Decode should succeed and produce the same number of inputs
        let result = decode_inputs(&encoded);
        assert!(result.is_ok(), "Should decode max input count");
        let (_, decoded_inputs) = result.unwrap();
        assert_eq!(
            decoded_inputs.len(),
            u8::MAX as usize,
            "Decoded input count should match max"
        );
    }

    #[test]
    fn test_encode_decode_max_outputs() {
        let note = Note::new(1000, ZkPublicKey::from(BigUint::from(42u64)));
        let outputs = [note; u8::MAX as usize];

        // Encode should succeed
        let encoded = encode_outputs(&outputs);
        assert!(
            !encoded.is_empty(),
            "Encoding max output count should produce some output"
        );

        // Decode should succeed and produce the same number of outputs
        let result = decode_outputs(&encoded);
        assert!(result.is_ok(), "Should decode max output count");
        let (_, decoded_outputs) = result.unwrap();
        assert_eq!(
            decoded_outputs.len(),
            u8::MAX as usize,
            "Decoded output count should match max"
        );
    }

    #[test]
    fn test_accept_max_input_output_counts() {
        // Test that input_count = MAX_INPUT_COUNT works
        let mut valid_input = Vec::new();
        valid_input.push(u8::MAX);

        // Add MAX_INPUT_COUNT field elements (each 32 bytes)
        for _ in 0..u8::MAX {
            valid_input.extend_from_slice(&[0x01; 32]);
        }

        let result = decode_inputs(&valid_input);
        assert!(result.is_ok(), "Should accept max input count");
        let (_, inputs) = result.unwrap();
        assert_eq!(inputs.len(), u8::MAX as usize);

        // Test that output_count = MAX_OUTPUT_COUNT works
        let mut valid_output = u8::MAX.to_le_bytes().to_vec();

        // Add MAX_OUTPUT_COUNT notes (each: 8 bytes value + 32 bytes key)
        for _ in 0..u8::MAX {
            valid_output.extend_from_slice(&42u64.to_le_bytes()); // value
            valid_output.extend_from_slice(&[0x02; 32]); // public key
        }

        let result = decode_outputs(&valid_output);
        assert!(result.is_ok(), "Should accept max output count");
        let (_, outputs) = result.unwrap();
        assert_eq!(outputs.len(), u8::MAX as usize);
    }
}
