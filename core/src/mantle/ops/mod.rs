pub mod channel;
pub mod leader_claim;
pub mod sdp;
pub mod transfer;

pub(crate) mod internal;

pub(crate) mod codec;
mod serde_;

use std::sync::LazyLock;

use channel::{
    channel_transfer::ChannelTransferOp, config::ChannelConfigOp, deposit::DepositOp,
    inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use nom::{
    IResult,
    error::{Error, ErrorKind},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{
    gas::{Gas, GasConstants},
    ops::{
        leader_claim::LeaderClaimOp,
        sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    },
};
use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        nom::{NomDecode, NomEncode},
        ops::{
            internal::{OpDe, OpSer},
            transfer::TransferOp,
        },
    },
    proofs::{
        channel_multi_sig_proof::ChannelMultiSigProof, leader_claim_proof::Groth16LeaderClaimProof,
    },
};

static OPERATION_ID_V1: LazyLock<Vec<u8>> = LazyLock::new(|| b"OPERATION_ID_V1".to_vec());

pub trait OpId {
    fn op_id(&self) -> Hash {
        let mut encoded_bytes = OPERATION_ID_V1.clone();
        encoded_bytes.extend(self.op_bytes());
        Hasher::digest(&encoded_bytes).into()
    }

    fn op_bytes(&self) -> Vec<u8>;
}

const TRANSFER: u8 = 0x00;
const CHANNEL_CONFIG: u8 = 0x10;
const INSCRIBE: u8 = 0x11;
const CHANNEL_DEPOSIT: u8 = 0x12;
const CHANNEL_WITHDRAW: u8 = 0x13;
const CHANNEL_TRANSFER: u8 = 0x14;
const SDP_DECLARE: u8 = 0x20;
const SDP_WITHDRAW: u8 = 0x21;
const SDP_ACTIVE: u8 = 0x22;
const LEADER_CLAIM: u8 = 0x30;

/// Core set of supported Mantle operations.
///
/// This type serves as the public-facing representation of [`OpSer`] and
/// [`OpDe`], delegating default serialization and deserialization to them.
///
/// Serialization and deserialization share a single [`serde_::OpWire`] wire
/// shape, which carries an `opcode` tag used to identify the correct variant.
/// Due to limitations in [`bincode`] and [`serde`]'s `#[serde(untagged)]`
/// enums, binary deserialization is routed through [`decode_op`] instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Op {
    ChannelInscribe(InscriptionOp),
    ChannelConfig(ChannelConfigOp),
    ChannelDeposit(DepositOp),
    ChannelWithdraw(ChannelWithdrawOp),
    ChannelTransfer(ChannelTransferOp),
    SDPDeclare(SDPDeclareOp),
    SDPWithdraw(SDPWithdrawOp),
    SDPActive(SDPActiveOp),
    LeaderClaim(LeaderClaimOp),
    Transfer(TransferOp),
}

/// Delegates serialization through the [`OpInternal`] representation.
impl Serialize for Op {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let op_ser = OpSer::from(self);
            op_ser.serialize(serializer)
        } else {
            let bytes = self.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}

/// Delegates deserialization through the [`OpDe`] representation.
///
/// If the deserializer is non-human-readable it falls back into custom
/// decoding via [`decode_op`]. Otherwise, it deserializes via [`OpDe`]'s
/// default behaviour.
impl<'de> Deserialize<'de> for Op {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            OpDe::deserialize(deserializer).map(Self::from)
        } else {
            let bytes = <Vec<u8>>::deserialize(deserializer)?;
            Self::decode(&bytes)
                .map(|(_, op)| op)
                .map_err(serde::de::Error::custom)
        }
    }
}

// Op = Opcode OpPayload
impl NomEncode for Op {
    fn encode(&self) -> Vec<u8> {
        let op_code = self.code();
        let mut bytes = op_code.encode();
        match self {
            Self::ChannelInscribe(op) => {
                bytes.extend(op.encode());
            }
            Self::ChannelConfig(op) => {
                bytes.extend(op.encode());
            }
            Self::ChannelDeposit(op) => {
                bytes.extend(op.encode());
            }
            Self::ChannelWithdraw(op) => {
                bytes.extend(op.encode());
            }
            Self::ChannelTransfer(op) => {
                bytes.extend(op.encode());
            }
            Self::SDPDeclare(op) => {
                bytes.extend(op.encode());
            }
            Self::SDPWithdraw(op) => {
                bytes.extend(op.encode());
            }
            Self::SDPActive(op) => {
                bytes.extend(op.encode());
            }
            Self::LeaderClaim(op) => {
                bytes.extend(op.encode());
            }
            Self::Transfer(op) => bytes.extend(op.encode()),
        }
        bytes
    }
}

impl NomDecode for Op {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (bytes, opcode) = u8::decode(bytes)?;

        match opcode {
            INSCRIBE => {
                InscriptionOp::decode(bytes).map(|(bytes, op)| (bytes, Self::ChannelInscribe(op)))
            }
            CHANNEL_CONFIG => {
                ChannelConfigOp::decode(bytes).map(|(bytes, op)| (bytes, Self::ChannelConfig(op)))
            }
            CHANNEL_DEPOSIT => {
                DepositOp::decode(bytes).map(|(bytes, op)| (bytes, Self::ChannelDeposit(op)))
            }
            CHANNEL_WITHDRAW => ChannelWithdrawOp::decode(bytes)
                .map(|(bytes, op)| (bytes, Self::ChannelWithdraw(op))),
            CHANNEL_TRANSFER => ChannelTransferOp::decode(bytes)
                .map(|(bytes, op)| (bytes, Self::ChannelTransfer(op))),
            SDP_DECLARE => {
                SDPDeclareOp::decode(bytes).map(|(bytes, op)| (bytes, Self::SDPDeclare(op)))
            }
            SDP_WITHDRAW => {
                SDPWithdrawOp::decode(bytes).map(|(bytes, op)| (bytes, Self::SDPWithdraw(op)))
            }
            SDP_ACTIVE => {
                SDPActiveOp::decode(bytes).map(|(bytes, op)| (bytes, Self::SDPActive(op)))
            }
            LEADER_CLAIM => {
                LeaderClaimOp::decode(bytes).map(|(bytes, op)| (bytes, Self::LeaderClaim(op)))
            }
            TRANSFER => TransferOp::decode(bytes).map(|(bytes, op)| (bytes, Self::Transfer(op))),
            _ => Err(nom::Err::Error(Error::new(bytes, ErrorKind::Fail))),
        }
    }
}

// We just check that the enum discriminant tag is encoded correctly, so a
// single fixture is fine here.
// TODO: Remove once the `NomCodec` macro supports enums.

impl Op {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ChannelInscribe(_) => "ChannelInscribe",
            Self::ChannelConfig(_) => "ChannelConfig",
            Self::ChannelDeposit(_) => "ChannelDeposit",
            Self::ChannelWithdraw(_) => "ChannelWithdraw",
            Self::ChannelTransfer(_) => "ChannelTransfer",
            Self::SDPDeclare(_) => "SDPDeclare",
            Self::SDPWithdraw(_) => "SDPWithdraw",
            Self::SDPActive(_) => "SDPActive",
            Self::LeaderClaim(_) => "LeaderClaim",
            Self::Transfer(_) => "Transfer",
        }
    }

    #[must_use]
    pub const fn execution_gas<Constants: GasConstants>(&self) -> Gas {
        match self {
            Self::ChannelInscribe(_) => Constants::CHANNEL_INSCRIBE,
            Self::ChannelConfig(_) => Constants::CHANNEL_CONFIG,
            Self::ChannelDeposit(_) => Constants::CHANNEL_DEPOSIT,
            Self::ChannelWithdraw(_) => Constants::CHANNEL_WITHDRAW,
            Self::ChannelTransfer(_) => Constants::CHANNEL_TRANSFER,
            Self::SDPDeclare(_) => Constants::SDP_DECLARE,
            Self::SDPWithdraw(_) => Constants::SDP_WITHDRAW,
            Self::SDPActive(_) => Constants::SDP_ACTIVE,
            Self::LeaderClaim(_) => Constants::LEADER_CLAIM,
            Self::Transfer(_) => Constants::TRANSFER,
        }
    }

    const fn code(&self) -> u8 {
        match self {
            Self::ChannelInscribe(_) => INSCRIBE,
            Self::ChannelConfig(_) => CHANNEL_CONFIG,
            Self::ChannelDeposit(_) => CHANNEL_DEPOSIT,
            Self::ChannelWithdraw(_) => CHANNEL_WITHDRAW,
            Self::ChannelTransfer(_) => CHANNEL_TRANSFER,
            Self::SDPDeclare(_) => SDP_DECLARE,
            Self::SDPWithdraw(_) => SDP_WITHDRAW,
            Self::SDPActive(_) => SDP_ACTIVE,
            Self::LeaderClaim(_) => LEADER_CLAIM,
            Self::Transfer(_) => TRANSFER,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpProof {
    Ed25519Sig(Ed25519Signature),
    ZkSig(ZkSignature),
    ZkAndEd25519Sigs {
        zk_sig: ZkSignature,
        ed25519_sig: Ed25519Signature,
    },
    PoC(Groth16LeaderClaimProof),
    ChannelMultiSigProof(ChannelMultiSigProof),
}

/// Mantle reference test-vector generators.
///
/// This module does not assert library behaviour: it emits reference test
/// vectors so that alternative implementations (e.g. the nim implementation)
/// can be checked for conformance against the canonical Rust encoding. Two
/// generators are provided:
///
/// - [`generate_op_id_test_vectors`]: for every [`Op`] variant, the `payload`
///   (the canonical operation encoding without the leading opcode byte, i.e.
///   exactly what [`OpId::op_bytes`] returns) and the resulting `op_id =
///   Blake2b-256(b"OPERATION_ID_V1" || payload)`. For the variants that
///   implement [`OpId`] (`Transfer`, `ChannelDeposit`, `ChannelWithdraw`,
///   `LeaderClaim`) the emitted `op_id` is asserted to equal `OpId::op_id`.
///
/// - [`generate_mantle_tx_hash_test_vectors`]: for an empty transaction and for
///   a transaction holding one of every operation, the `encoding` (the
///   canonical transaction encoding, i.e. `MantleTx::encode`, which is an
///   op-count byte followed by each `opcode || op_payload`) and the resulting
///   `tx_hash = Blake2b-256(b"MANTLE_TXHASH_V1" || encoding)`. The emitted hash
///   is asserted to equal `MantleTx::hash`.
///
/// All deterministic inputs are fixed, so the vectors are stable across runs.
/// The tests are `#[ignore]`d so they are skipped by `cargo test
/// --all-features`. Run them on demand with:
/// `cargo test -p logos-blockchain-core mantle_test_vectors -- --ignored
/// --nocapture`
#[cfg(test)]
mod mantle_test_vectors {
    use lb_blend_proofs::{
        quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
        selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
    };
    use lb_cryptarchia_engine::Epoch;
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};
    use lb_poseidon2::Fr;

    use super::*;
    use crate::{
        mantle::{
            MantleTx, Note,
            channel::{SlotTimeframe, SlotTimeout},
            ledger::{Inputs, NoteId, Outputs},
            ops::channel::{ChannelId, MsgId, config::Keys, deposit::Metadata},
            traits::Hashable as _,
            transactions::Ops,
        },
        sdp::{
            ActiveMessage, ActivityMetadata, DeclarationId, DeclarationMessage, Locator,
            ProviderId, ServiceType, WithdrawMessage, blend::ActivityProof,
        },
    };

    fn ed25519_pk(seed: u8) -> channel::Ed25519PublicKey {
        Ed25519Key::from_bytes(&[seed; 32]).public_key()
    }

    fn zk_pk(seed: u64) -> ZkPublicKey {
        ZkPublicKey::from(Fr::from(seed))
    }

    /// One deterministic instance of every [`Op`] variant.
    fn sample_ops() -> Vec<Op> {
        let activity = ActivityProof {
            epoch: Epoch::new(10),
            signing_key: ed25519_pk(1),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([2u8; PROOF_OF_QUOTA_SIZE])
                .into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked(
                [3u8; PROOF_OF_SELECTION_SIZE],
            )
            .into(),
        };

        vec![
            // Transfer (0x00)
            Op::Transfer(TransferOp::new(
                Inputs::new([NoteId(Fr::from(1u64)), NoteId(Fr::from(2u64))]),
                Outputs::new([Note::new(3, zk_pk(4)), Note::new(5, zk_pk(6))]),
            )),
            // ChannelConfig (0x10)
            Op::ChannelConfig(ChannelConfigOp {
                channel: ChannelId::from([7u8; 32]),
                keys: Keys::try_from(vec![ed25519_pk(8), ed25519_pk(9)]).unwrap(),
                posting_timeframe: SlotTimeframe::from(10u32),
                posting_timeout: SlotTimeout::from(11u32),
                configuration_threshold: 12,
                transfer_threshold: 13,
            }),
            // ChannelInscribe (0x11)
            Op::ChannelInscribe(InscriptionOp {
                channel_id: ChannelId::from([14u8; 32]),
                inscription: b"hello logos".into(),
                parent: MsgId::root(),
                signer: ed25519_pk(15),
            }),
            // ChannelDeposit (0x12)
            Op::ChannelDeposit(DepositOp {
                channel_id: ChannelId::from([16u8; 32]),
                inputs: Inputs::new([NoteId(Fr::from(17u64))]),
                metadata: Metadata::try_from(b"deposit-metadata".to_vec()).unwrap(),
            }),
            // ChannelWithdraw (0x13)
            Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id: ChannelId::from([18u8; 32]),
                inputs: Inputs::new([NoteId(Fr::from(19u64))]),
            }),
            // ChannelTransfer (0x14)
            Op::ChannelTransfer(ChannelTransferOp {
                channel_id: ChannelId::from([20u8; 32]),
                inputs: Inputs::new([NoteId(Fr::from(21u64))]),
                outputs: Outputs::new([Note::new(22, zk_pk(23))]),
            }),
            // SDPDeclare (0x20)
            Op::SDPDeclare(DeclarationMessage {
                service_type: ServiceType::BlendNetwork,
                locators: "/ip4/127.0.0.1/udp/3000/quic-v1"
                    .parse::<Locator>()
                    .unwrap()
                    .into(),
                provider_id: ProviderId(ed25519_pk(24)),
                zk_id: zk_pk(25),
                locked_note_id: NoteId(Fr::from(26u64)),
            }),
            // SDPWithdraw (0x21)
            Op::SDPWithdraw(WithdrawMessage {
                declaration_id: DeclarationId([27u8; 32]),
                locked_note_id: NoteId(Fr::from(28u64)),
                nonce: 29,
            }),
            // SDPActive (0x22)
            Op::SDPActive(ActiveMessage {
                declaration_id: DeclarationId([30u8; 32]),
                nonce: 31,
                metadata: ActivityMetadata::Blend(Box::new(activity)),
            }),
            // LeaderClaim (0x30)
            Op::LeaderClaim(LeaderClaimOp {
                rewards_root: Fr::from(32u64).into(),
                voucher_nullifier: Fr::from(33u64).into(),
                pk: zk_pk(34),
            }),
        ]
    }

    /// `op_id = blake2b256("OPERATION_ID_V1" || op_payload_bytes)`
    /// where `op_payload_bytes` is the canonical operation encoding without the
    /// 1-byte opcode tag (i.e. exactly what `OpId::op_bytes` returns).
    fn op_id_from_payload(payload: &[u8]) -> [u8; 32] {
        let mut preimage = OPERATION_ID_V1.clone();
        preimage.extend_from_slice(payload);
        Hasher::digest(&preimage).into()
    }

    /// `tx_hash = blake2b256("MANTLE_TXHASH_V1" || tx_payload_bytes)`
    /// where `tx_payload_bytes` is the canonical transaction encoding (i.e.
    /// `MantleTx::encode`).
    fn tx_hash_from_payload(payload: &[u8]) -> [u8; 32] {
        let mut preimage = b"MANTLE_TXHASH_V1".to_vec();
        preimage.extend_from_slice(payload);
        Hasher::digest(&preimage).into()
    }

    fn print_op_vector(op: &Op) {
        let payload = &op.encode()[1..]; // == OpId::op_bytes()
        let op_id = op_id_from_payload(payload);

        println!("{}", op.as_str());
        println!("payload {}", hex::encode(payload));
        println!("op_id   {}", hex::encode(op_id));
        println!();
    }

    fn print_tx_vector(label: &str, tx: &MantleTx) {
        let payload = tx.encode();
        let tx_hash = tx_hash_from_payload(&payload);
        // The hand-rolled computation must match the production `hash()`.
        assert_eq!(tx.hash().0, tx_hash);

        println!("{label}");
        println!("encoding {}", hex::encode(&payload));
        println!("tx_hash  {}", hex::encode(tx_hash));
        println!();
    }

    /// Generates (and prints) the Op ID test vectors for every mantle
    /// operation. Ignored by default so it never runs under `cargo test
    /// --all-features`; invoke explicitly with `--ignored --nocapture` to
    /// regenerate the vectors.
    #[test]
    #[ignore = "generates OpId test vectors on demand; run with --ignored --nocapture"]
    fn generate_op_id_test_vectors() {
        println!();
        for op in &sample_ops() {
            print_op_vector(op);
            // Cross-check against the production trait where it is implemented.
            match op {
                Op::Transfer(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
                Op::ChannelDeposit(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
                Op::ChannelTransfer(o) => {
                    assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes()));
                }
                Op::ChannelWithdraw(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
                Op::LeaderClaim(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
                _ => {}
            }
        }
    }

    /// Generates (and prints) the Mantle transaction-hash test vectors for an
    /// empty transaction and for a transaction holding one of every operation.
    /// Ignored by default so it never runs under `cargo test --all-features`;
    /// invoke explicitly with `--ignored --nocapture` to regenerate the
    /// vectors.
    #[test]
    #[ignore = "generates Mantle tx-hash test vectors on demand; run with --ignored --nocapture"]
    fn generate_mantle_tx_hash_test_vectors() {
        println!();
        // Empty transaction (zero operations).
        print_tx_vector("empty (0 ops)", &MantleTx(Ops::new_unchecked(vec![])));

        // Transaction holding one of every operation.
        print_tx_vector(
            "one of each operation (9 ops)",
            &MantleTx(Ops::new_unchecked(sample_ops())),
        );
    }
}
