use core::fmt::{self, Debug, Formatter};

use blake2::Digest as _;
use lb_codec::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::Slot;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod fixtures;

use crate::{
    codec::SerializeOp as _,
    crypto::Hasher,
    mantle::transactions::GenesisTx,
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof as _},
    utils::{display_hex_bytes_newtype, serde_bytes_newtype},
};

pub const BEDROCK_VERSION: u8 = 1;

#[derive(Clone, Eq, PartialEq, Copy, Hash, PartialOrd, Ord, BinaryCodec)]
pub struct HeaderId([u8; 32]);

impl Debug for HeaderId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "HeaderId({})", hex::encode(self.0))
    }
}

#[derive(Clone, Eq, PartialEq, Copy, Hash, BinaryCodec)]
pub struct ContentId([u8; 32]);

impl Debug for ContentId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "ContentId({})", hex::encode(self.0))
    }
}

#[derive(Clone, Eq, PartialEq, Copy)]
pub struct Nonce([u8; 32]);

impl Debug for Nonce {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Nonce({})", hex::encode(self.0))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Copy)]
#[repr(u8)]
pub enum Version {
    Bedrock = BEDROCK_VERSION,
}

impl Version {
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for Version {
    type Error = std::io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            BEDROCK_VERSION => Ok(Self::Bedrock),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid version [{value}]"),
            )),
        }
    }
}

impl TryFrom<&str> for Version {
    type Error = std::io::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "bedrock" => Ok(Self::Bedrock),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid version [{value}]"),
            )),
        }
    }
}

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(format!("{self:?}").as_str())
        } else {
            serializer.serialize_u8(self.as_byte())
        }
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let s = String::deserialize(deserializer)?;
            Self::try_from(s.as_str()).map_err(serde::de::Error::custom)
        } else {
            Self::try_from(<u8>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
        }
    }
}

impl BinaryEncode for Version {
    fn encoded_length(&self) -> usize {
        self.as_byte().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.as_byte());
    }
}

impl BinaryDecode for Version {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, version) = u8::decode(input, context)?;
        let version = Self::try_from(version)
            .map_err(|_| DecodeError::unknown_discriminant::<Self>(u64::from(version)))?;
        Ok((input, version))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BinaryCodec)]
pub struct Header {
    version: Version,
    parent_block: HeaderId,
    slot: Slot,
    body_root: ContentId,
    proof_of_leadership: Groth16LeaderProof,
}

impl Header {
    #[must_use]
    pub const fn version(&self) -> &Version {
        &self.version
    }

    #[must_use]
    pub const fn parent(&self) -> HeaderId {
        self.parent_block
    }

    fn update_hasher(&self, h: &mut Hasher) {
        h.update(b"BLOCK_ID_V1");
        h.update(self.version.as_byte().to_le_bytes());
        h.update(self.parent_block.0);
        h.update(self.slot.to_le_bytes());
        h.update(self.body_root.0);
        h.update(self.proof_of_leadership.voucher_cm().to_bytes());
        h.update(fr_to_bytes(&self.proof_of_leadership.entropy()));
        h.update(self.proof_of_leadership.proof().to_bytes());
        h.update(self.proof_of_leadership.leader_key().to_bytes());
    }

    #[must_use]
    pub fn id(&self) -> HeaderId {
        let mut h = Hasher::new();
        self.update_hasher(&mut h);
        HeaderId(h.finalize().into())
    }

    #[must_use]
    pub const fn leader_proof(&self) -> &Groth16LeaderProof {
        &self.proof_of_leadership
    }

    #[must_use]
    pub const fn body_root(&self) -> &ContentId {
        &self.body_root
    }

    #[must_use]
    pub const fn slot(&self) -> Slot {
        self.slot
    }

    pub fn sign(&self, signing_key: &Ed25519Key) -> Result<Ed25519Signature, crate::block::Error> {
        let header_bytes = self.to_bytes()?;
        Ok(signing_key.sign_payload(&header_bytes))
    }

    #[must_use]
    pub const fn parent_block(&self) -> HeaderId {
        self.parent_block
    }

    #[must_use]
    pub const fn new(
        parent_block: HeaderId,
        body_root: ContentId,
        slot: Slot,
        proof_of_leadership: Groth16LeaderProof,
    ) -> Self {
        Self {
            version: Version::Bedrock,
            parent_block,
            slot,
            body_root,
            proof_of_leadership,
        }
    }

    #[must_use]
    pub fn genesis(tx: &GenesisTx) -> Self {
        Self::new(
            HeaderId([0; 32]),
            crate::block::body_root(&crate::block::UncleHeaders::empty(), &[tx]),
            Slot::from(0u64),
            Groth16LeaderProof::genesis(),
        )
    }
}

impl From<[u8; 32]> for HeaderId {
    fn from(id: [u8; 32]) -> Self {
        Self(id)
    }
}

impl From<HeaderId> for [u8; 32] {
    fn from(id: HeaderId) -> Self {
        id.0
    }
}

impl TryFrom<&[u8]> for HeaderId {
    type Error = Error;

    fn try_from(slice: &[u8]) -> Result<Self, Self::Error> {
        if slice.len() != 32 {
            return Err(Error::InvalidHeaderIdSize(slice.len()));
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(slice);
        Ok(Self::from(id))
    }
}

impl AsRef<[u8]> for HeaderId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<[u8; 32]> for ContentId {
    fn from(id: [u8; 32]) -> Self {
        Self(id)
    }
}

impl From<ContentId> for [u8; 32] {
    fn from(id: ContentId) -> Self {
        id.0
    }
}

display_hex_bytes_newtype!(HeaderId);
display_hex_bytes_newtype!(ContentId);
display_hex_bytes_newtype!(Nonce);

serde_bytes_newtype!(HeaderId, 32);
serde_bytes_newtype!(ContentId, 32);
serde_bytes_newtype!(Nonce, 32);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid header id size: {0}")]
    InvalidHeaderIdSize(usize),
}

#[test]
fn test_serde() {
    use crate::codec::{DeserializeOp as _, SerializeOp as _};
    let header = HeaderId([0; 32]);
    assert_eq!(
        HeaderId::from_bytes(
            &header
                .to_bytes()
                .expect("HeaderId should be able to be serialized")
        )
        .unwrap(),
        HeaderId([0; 32])
    );
}

#[test]
fn test_serde_json_roundtrip() {
    let header = HeaderId([0xAB; 32]);
    let json = serde_json::to_string(&header).unwrap();

    assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
    assert_eq!(serde_json::from_str::<HeaderId>(&json).unwrap(), header);
}

#[test]
fn test_serde_json_rejects_oversized_hex() {
    let json = format!("\"{}\"", "ab".repeat(33));
    assert!(serde_json::from_str::<HeaderId>(&json).is_err());
}

/// Body-root / `HeaderId` test-vector generator.
///
/// This module does not assert library behaviour: it *emits* reference test
/// vectors for the block `body_root` computation and the resulting
/// [`HeaderId`], so that alternative implementations (e.g. the nim
/// implementation) can be checked for conformance against the canonical Rust
/// encoding.
///
/// It emits five vectors:
/// - **empty block**: tx root of a block with no transactions (`Merkle([]) ==
///   [0u8; 32]`);
/// - **one tx per op kind**: a block whose transactions each carry a single
///   operation, one per distinct mantle [`Op`] variant; for it we print every
///   leaf (`tx_hash`) and the resulting tx root;
/// - **`body_root` without uncles**: over an empty `uncle_headers` list and the
///   previous tx root;
/// - **`body_root` with uncles**: the same tx root over two uncle headers;
/// - **`HeaderId`**: a header reusing the `body_root` without uncles, with a
///   fixed parent, slot and a deterministic genesis proof of leadership, for
///   which we print the inputs and the resulting `HeaderId`.
///
/// `transaction_root = Merkle(blake2b256(b"MANTLE_TXHASH_V1" || tx_bytes)
/// leaves)`, where the Merkle tree pads the leaf set to the next power of two
/// with all-zero leaves and hashes inner nodes as `blake2b256(left || right)`.
///
/// `body_root = blake2b256( b"BODY_ROOT_V1" || uncle_headers ||
/// transactions_root )`, where `uncle_headers` is the list encoding: a 1-byte
/// little-endian element count followed by that many fixed 361-byte entries.
///
/// `HeaderId (block_id) = blake2b256( b"BLOCK_ID_V1" || bedrock_version (1B)
/// ||` `parent_block (32B) || slot_le (8B) || body_root (32B) ||
/// leader_voucher` `(32B) || entropy_contribution (32B) || proof (128B) ||
/// leader_key (32B) )`.
///
/// The test is `#[ignore]`d so it is skipped by `cargo test --all-features`.
/// Run it on demand with:
/// `cargo test -p logos-blockchain-core body_root_test_vectors -- --ignored
/// --nocapture`
#[cfg(test)]
mod body_root_test_vectors {
    use lb_blend_proofs::{
        quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
        selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
    };
    use lb_cryptarchia_engine::Epoch;
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_poseidon2::Fr;

    use super::*;
    use crate::{
        block::{SignedHeader, UncleHeaders},
        mantle::{
            Note, Op, RawMantleTx,
            channel::{SlotTimeframe, SlotTimeout},
            ledger::{Inputs, NoteId, Outputs},
            ops::{
                channel::{
                    ChannelId, Ed25519PublicKey, MsgId,
                    channel_transfer::ChannelTransferOp,
                    config::{ChannelConfigOp, Keys},
                    deposit::{DepositOp, Metadata},
                    inscribe::InscriptionOp,
                    withdraw::ChannelWithdrawOp,
                },
                leader_claim::{LeaderClaimOp, VoucherCm},
                transfer::TransferOp,
            },
            traits::Hashable as _,
            transactions::Ops,
        },
        sdp::{
            ActiveMessage, ActivityMetadata, DeclarationId, DeclarationMessage, Locator,
            ProviderId, ServiceType, WithdrawMessage, blend::ActivityProof,
        },
        utils::merkle,
    };

    fn ed25519_pk(seed: u8) -> Ed25519PublicKey {
        Ed25519Key::from_bytes(&[seed; 32]).public_key()
    }

    fn zk_pk(seed: u64) -> ZkPublicKey {
        ZkPublicKey::from(Fr::from(seed))
    }

    fn tx(op: Op) -> RawMantleTx {
        RawMantleTx(Ops::new_unchecked(vec![op]))
    }

    /// Builds one transaction per distinct mantle operation kind, each carrying
    /// a single operation. The instances mirror those used by the `OpId` test
    /// vectors so the two vector sets stay consistent.
    fn one_tx_per_op() -> Vec<(&'static str, RawMantleTx)> {
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
            (
                "Transfer",
                tx(Op::Transfer(TransferOp::new(
                    Inputs::new([NoteId(Fr::from(1u64)), NoteId(Fr::from(2u64))]),
                    Outputs::new([Note::new(3, zk_pk(4)), Note::new(5, zk_pk(6))]),
                ))),
            ),
            // ChannelConfig (0x10)
            (
                "ChannelConfig",
                tx(Op::ChannelConfig(ChannelConfigOp {
                    channel: ChannelId::from([7u8; 32]),
                    parent: MsgId::root(),
                    keys: Keys::try_from(vec![ed25519_pk(8), ed25519_pk(9)]).unwrap(),
                    posting_timeframe: SlotTimeframe::from(10u32),
                    posting_timeout: SlotTimeout::from(11u32),
                    configuration_threshold: 12,
                    transfer_threshold: 13,
                })),
            ),
            // ChannelInscribe (0x11)
            (
                "ChannelInscribe",
                tx(Op::ChannelInscribe(InscriptionOp {
                    channel_id: ChannelId::from([14u8; 32]),
                    inscription: b"hello logos".into(),
                    parent: MsgId::root(),
                    signer: ed25519_pk(15),
                })),
            ),
            // ChannelDeposit (0x12)
            (
                "ChannelDeposit",
                tx(Op::ChannelDeposit(DepositOp {
                    channel_id: ChannelId::from([16u8; 32]),
                    inputs: Inputs::new([NoteId(Fr::from(17u64))]),
                    metadata: Metadata::try_from(b"deposit-metadata".to_vec()).unwrap(),
                })),
            ),
            // ChannelWithdraw (0x13)
            (
                "ChannelWithdraw",
                tx(Op::ChannelWithdraw(ChannelWithdrawOp {
                    channel_id: ChannelId::from([18u8; 32]),
                    inputs: Inputs::new([NoteId(Fr::from(19u64))]),
                })),
            ),
            // ChannelTransfer (0x14)
            (
                "ChannelTransfer",
                tx(Op::ChannelTransfer(ChannelTransferOp {
                    channel_id: ChannelId::from([20u8; 32]),
                    inputs: Inputs::new([NoteId(Fr::from(21u64))]),
                    outputs: Outputs::new([Note::new(22, zk_pk(23))]),
                })),
            ),
            // SDPDeclare (0x20)
            (
                "SDPDeclare",
                tx(Op::SDPDeclare(DeclarationMessage {
                    service_type: ServiceType::BlendNetwork,
                    locators: "/ip4/127.0.0.1/udp/3000/quic-v1"
                        .parse::<Locator>()
                        .unwrap()
                        .into(),
                    provider_id: ProviderId(ed25519_pk(24)),
                    zk_id: zk_pk(25),
                    locked_note_id: NoteId(Fr::from(26u64)),
                })),
            ),
            // SDPWithdraw (0x21)
            (
                "SDPWithdraw",
                tx(Op::SDPWithdraw(WithdrawMessage {
                    declaration_id: DeclarationId([27u8; 32]),
                    locked_note_id: NoteId(Fr::from(28u64)),
                    nonce: 29,
                })),
            ),
            // SDPActive (0x22)
            (
                "SDPActive",
                tx(Op::SDPActive(ActiveMessage {
                    declaration_id: DeclarationId([30u8; 32]),
                    nonce: 31,
                    metadata: ActivityMetadata::Blend(Box::new(activity)),
                })),
            ),
            // LeaderClaim (0x30)
            (
                "LeaderClaim",
                tx(Op::LeaderClaim(LeaderClaimOp {
                    rewards_root: Fr::from(32u64).into(),
                    voucher_nullifier: Fr::from(33u64).into(),
                    pk: zk_pk(34),
                })),
            ),
        ]
    }

    fn uncle(byte: u8) -> SignedHeader {
        let signing_key = Ed25519Key::from_bytes(&[byte; 32]);
        let header = Header::new(
            HeaderId([byte; 32]),
            ContentId([byte; 32]),
            Slot::from(u64::from(byte)),
            Groth16LeaderProof::from_parts(
                lb_pol::PoLProof::from_bytes(&[byte; 128]),
                Fr::from(u64::from(byte)),
                signing_key.public_key(),
                VoucherCm::from(Fr::from(u64::from(byte))),
            ),
        );
        let signature = header.sign(&signing_key).expect("header serializes");
        SignedHeader::new(header, signature)
    }

    /// Generates (and prints) the `body_root` / `HeaderId` test vectors.
    /// Ignored by default so it never runs under `cargo test --all-features`;
    /// invoke explicitly with `--ignored --nocapture` to regenerate the
    /// vectors.
    #[test]
    #[ignore = "generates body_root/HeaderId test vectors on demand; run with --ignored --nocapture"]
    #[expect(clippy::too_many_lines, reason = "a flat list of printed vectors")]
    fn generate_body_root_test_vectors() {
        println!();
        println!(
            "transactions_root = Merkle( blake2b256(b\"MANTLE_TXHASH_V1\" || tx_bytes) leaves )"
        );
        println!(
            "body_root  = blake2b256( b\"BODY_ROOT_V1\" || uncle_headers || transactions_root )"
        );
        println!(
            "block_id   = blake2b256( b\"BLOCK_ID_V1\" || bedrock_version || parent_block || \
             slot_le || body_root || leader_voucher || entropy_contribution || proof || \
             leader_key )"
        );

        // 1. Empty block: no transactions.
        let empty: Vec<RawMantleTx> = vec![];
        println!("================================================================");
        println!("vector 1  : empty block (0 transactions)");
        println!(
            "{:20}: {}",
            "transactions_root",
            hex::encode(merkle::calculate_transactions_root(&empty))
        );

        // 2. One transaction per operation kind (one op each).
        let txs_with_names = one_tx_per_op();
        let txs: Vec<RawMantleTx> = txs_with_names.iter().map(|(_, tx)| tx.clone()).collect();
        println!("================================================================");
        println!(
            "vector 2  : one transaction per op kind ({} transactions)",
            txs.len()
        );
        for (i, (name, tx)) in txs_with_names.iter().enumerate() {
            println!("leaf[{i}]   : {} (op: {})", hex::encode(tx.hash().0), name);
        }
        println!(
            "{:20}: {}",
            "transactions_root",
            hex::encode(merkle::calculate_transactions_root(&txs))
        );

        // 3. `body_root` over vector 2's transactions and no uncles. This is the value
        //    vector 5's header carries.
        let body_root = crate::block::body_root(&UncleHeaders::empty(), &txs);
        println!("================================================================");
        println!("vector 3  : body_root without uncles, over vector 2's transactions");
        println!("{:20}: 00 (empty list)", "uncle_headers");
        println!("{:20}: {}", "body_root", hex::encode(body_root.0));

        // 4. The same transactions with two carried uncles. Nothing downstream consumes
        //    this vector; it is here so that another implementation can check its
        //    `uncle_headers` encoding, signatures included.
        let uncles = UncleHeaders::new([uncle(0x66), uncle(0x77)]);
        println!("================================================================");
        println!("vector 4  : body_root with 2 uncles, over vector 2's transactions");
        println!("{:20}: {:02x}", "uncle_count", uncles.len());
        for (index, uncle) in uncles.iter().enumerate() {
            println!(
                "{:20}: {}",
                format!("uncle_headers[{index}]"),
                hex::encode(uncle.to_bytes().expect("uncle serializes"))
            );
        }
        println!(
            "{:20}: {}",
            "body_root",
            hex::encode(crate::block::body_root(&uncles, &txs).0)
        );

        // 5. HeaderId reusing vector 3's body_root. Every field is given a distinct
        //    value so that a field-transposition bug in another implementation cannot
        //    be masked by shared bytes. The proof is a synthetic (non-verifying) proof
        //    built only to exercise the hash.
        let parent_block = HeaderId([0x11u8; 32]);
        let slot = Slot::from(42u64);
        let proof = Groth16LeaderProof::from_parts(
            lb_pol::PoLProof::from_bytes(&[0x22u8; 128]),
            Fr::from(0x5555u64), // entropy_contribution
            Ed25519Key::from_bytes(&[0x33u8; 32]).public_key(), // leader_key
            VoucherCm::from(Fr::from(0x4444u64)), // leader_voucher
        );
        let header = Header::new(parent_block, body_root, slot, proof);
        let header_id = header.id();

        // Self-check: recompute the preimage from the public accessors and make
        // sure it matches `Header::id()` (the exact byte layout is documented in
        // the module-level comment).
        let proof = header.leader_proof();
        let mut h = Hasher::new();
        h.update(b"BLOCK_ID_V1");
        h.update(Version::Bedrock.as_byte().to_le_bytes());
        h.update(parent_block.0);
        h.update(slot.to_le_bytes());
        h.update(body_root.0);
        h.update(proof.voucher_cm().to_bytes());
        h.update(fr_to_bytes(&proof.entropy()));
        h.update(proof.proof().to_bytes());
        h.update(proof.leader_key().to_bytes());
        let manual: [u8; 32] = h.finalize().into();
        assert_eq!(
            manual, header_id.0,
            "manual preimage must match Header::id()"
        );

        println!("================================================================");
        // Field labels match the names in the `block_id`/`Header` specification.
        println!("vector 5  : HeaderId (block_id) reusing vector 3's body_root");
        println!(
            "{:20}: {:02x}",
            "bedrock_version",
            Version::Bedrock.as_byte()
        );
        println!("{:20}: {}", "parent_block", hex::encode(parent_block.0));
        println!("{:20}: {}", "slot", u64::from(slot));
        println!("{:20}: {}", "body_root", hex::encode(body_root.0));
        // proof_of_leadership fields (here, the deterministic genesis proof).
        println!(
            "{:20}: {}",
            "leader_voucher",
            hex::encode(proof.voucher_cm().to_bytes())
        );
        println!(
            "{:20}: {}",
            "entropy_contribution",
            hex::encode(fr_to_bytes(&proof.entropy()))
        );
        println!("{:20}: {}", "proof", hex::encode(proof.proof().to_bytes()));
        println!(
            "{:20}: {}",
            "leader_key",
            hex::encode(proof.leader_key().to_bytes())
        );
        println!("{:20}: {}", "block_id", hex::encode(header_id.0));
        println!("================================================================");
    }
}
