use core::fmt::{self, Debug, Formatter};

use ark_ff::PrimeField as _;
use blake2::Digest as _;
#[cfg(test)]
use lb_binary_codec::bincode::SerializeOp as _;
use lb_binary_codec::{
    bincode::BoundedSerializeOp,
    canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError},
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Fr, fr_to_bytes};
use serde::{Deserialize, Serialize};

mod fixtures;

use crate::{
    crypto::Hasher,
    mantle::{Value, notes::NoteCommitment, transactions::GenesisTx},
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof, LeaderPublic},
    utils::{display_hex_bytes_newtype, serde_bytes_newtype},
};

pub const HEADER_BINCODE_SIZE: usize = <Slot as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + <HeaderId as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + <ContentId as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + <Groth16LeaderProof as BoundedSerializeOp>::MAX_ENCODED_SIZE;

#[derive(Clone, Eq, PartialEq, Copy, Hash, PartialOrd, Ord, BinaryCodec)]
pub struct HeaderId([u8; 32]);

impl HeaderId {
    /// The fixed-size canonical representation of a header identifier.
    pub const CANONICAL_ENCODED_SIZE: usize = 32;
}

impl Debug for HeaderId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "HeaderId({})", hex::encode(self.0))
    }
}

#[derive(Clone, Eq, PartialEq, Copy, Hash, BinaryCodec)]
pub struct ContentId([u8; 32]);

impl ContentId {
    /// The fixed-size canonical representation of a content identifier.
    pub const CANONICAL_ENCODED_SIZE: usize = 32;
}

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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Header {
    slot: Slot,
    parent_block: HeaderId,
    body_root: ContentId,
    proof_of_leadership: Groth16LeaderProof,
}

impl BinaryEncode for Header {
    fn encoded_length(&self) -> usize {
        let Self {
            slot,
            parent_block,
            body_root,
            proof_of_leadership,
        } = self;

        BEDROCK_VERSION.encoded_length()
            + slot.encoded_length()
            + parent_block.encoded_length()
            + body_root.encoded_length()
            + proof_of_leadership.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            slot,
            parent_block,
            body_root,
            proof_of_leadership,
        } = self;

        // The wire layout leads with `bedrock_version`, which tells a node which
        // rules parse the rest of the header.
        BEDROCK_VERSION.encode_into(out);
        parent_block.encode_into(out);
        slot.encode_into(out);
        body_root.encode_into(out);
        proof_of_leadership.encode_into(out);
    }
}

impl BinaryDecode for Header {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, bedrock_version) = u8::decode(input, context)?;
        // The block ID and `pol_msg` hash this constant.
        if bedrock_version != BEDROCK_VERSION {
            return Err(DecodeError::invalid_value::<Self>(
                "unsupported bedrock_version",
            ));
        }
        let (input, parent_block) = HeaderId::decode(input, context)?;
        let (input, slot) = Slot::decode(input, context)?;
        let (input, body_root) = ContentId::decode(input, context)?;
        let (input, proof_of_leadership) = Groth16LeaderProof::decode(input, context)?;

        Ok((
            input,
            Self {
                slot,
                parent_block,
                body_root,
                proof_of_leadership,
            },
        ))
    }
}

impl Header {
    /// The fixed-size canonical representation of a header.
    pub const CANONICAL_ENCODED_SIZE: usize = size_of::<u8>() // bedrock_version
        + Slot::CANONICAL_ENCODED_SIZE
        + HeaderId::CANONICAL_ENCODED_SIZE
        + ContentId::CANONICAL_ENCODED_SIZE
        + Groth16LeaderProof::CANONICAL_ENCODED_SIZE;

    /// Reads the slot of the encoded header `input` opens with, leaving the
    /// rest undecoded. The version is not checked, [`Header::decode`] does it.
    pub fn peek_slot(input: &[u8]) -> Result<Slot, DecodeError> {
        let (input, _bedrock_version) = u8::decode(input, &())?;
        let (input, _parent_block) = HeaderId::decode(input, &())?;
        Slot::peek_decode(input, &())
    }

    #[must_use]
    pub const fn parent(&self) -> HeaderId {
        self.parent_block
    }

    fn update_hasher(&self, h: &mut Hasher) {
        h.update(b"BLOCK_ID_V1");
        h.update([BEDROCK_VERSION]);
        h.update(self.parent_block.0);
        h.update(self.slot.to_le_bytes());
        h.update(self.body_root.0);
        // The proof is left out, a Groth16 proof can be re-randomized.
        h.update(fr_to_bytes(&self.proof_of_leadership.reward_commitment().0));
        h.update(self.proof_of_leadership.reward_value().to_le_bytes());
        h.update(fr_to_bytes(&self.proof_of_leadership.entropy()));
    }

    #[must_use]
    pub fn id(&self) -> HeaderId {
        let mut h = Hasher::new();
        self.update_hasher(&mut h);
        HeaderId(h.finalize().into())
    }

    #[must_use]
    pub fn pol_msg(&self) -> Fr {
        pol_msg(
            &self.parent_block,
            self.slot,
            &self.body_root,
            &self.proof_of_leadership.reward_commitment(),
            self.proof_of_leadership.reward_value(),
        )
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
            slot,
            parent_block,
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

impl LeaderProof for Header {
    fn verify(&self, public_inputs: &LeaderPublic) -> bool {
        self.proof_of_leadership
            .verify(public_inputs, self.pol_msg())
    }

    fn verify_genesis(&self) -> bool {
        self.proof_of_leadership.verify_genesis()
    }

    fn entropy(&self) -> Fr {
        self.proof_of_leadership.entropy()
    }

    fn reward_commitment(&self) -> NoteCommitment {
        self.proof_of_leadership.reward_commitment()
    }

    fn reward_value(&self) -> Value {
        self.proof_of_leadership.reward_value()
    }
}

// The spec's header `bedrock_version`, absorbed by the block ID and `pol_msg`.
pub const BEDROCK_VERSION: u8 = 1;

// Every header field except the proof and the entropy contribution.
#[must_use]
pub fn pol_msg(
    parent_block: &HeaderId,
    slot: Slot,
    body_root: &ContentId,
    reward_commitment: &NoteCommitment,
    reward_value: Value,
) -> Fr {
    let mut h = Hasher::new();
    h.update(b"POL_MSG_V1");
    h.update([BEDROCK_VERSION]);
    h.update(parent_block.0);
    h.update(slot.to_le_bytes());
    h.update(body_root.0);
    h.update(fr_to_bytes(&reward_commitment.0));
    h.update(reward_value.to_le_bytes());
    Fr::from_le_bytes_mod_order(&h.finalize())
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

impl AsRef<[u8; 32]> for ContentId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl AsRef<[u8; 32]> for Nonce {
    fn as_ref(&self) -> &[u8; 32] {
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

impl BoundedSerializeOp for HeaderId {
    type Bytes = [u8; 32];
}

impl BoundedSerializeOp for ContentId {
    type Bytes = [u8; 32];
}

impl BoundedSerializeOp for Nonce {
    type Bytes = [u8; 32];
}

impl BoundedSerializeOp for Header {
    type Bytes = [u8; HEADER_BINCODE_SIZE];
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid header id size: {0}")]
    InvalidHeaderIdSize(usize),
}

#[test]
fn test_serde() {
    use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
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
fn fixed_size_bincode_serialization_matches_for_header_types() {
    use lb_binary_codec::canonical::CodecExamples as _;

    let header_id = HeaderId([0x11; 32]);
    let content_id = ContentId([0x22; 32]);
    let nonce = Nonce([0x33; 32]);

    for (ordinary, bounded) in [
        (
            header_id.to_bytes().unwrap(),
            header_id.to_bounded_bytes().unwrap().to_vec(),
        ),
        (
            content_id.to_bytes().unwrap(),
            content_id.to_bounded_bytes().unwrap().to_vec(),
        ),
        (
            nonce.to_bytes().unwrap(),
            nonce.to_bounded_bytes().unwrap().to_vec(),
        ),
    ] {
        assert_eq!(ordinary.len(), 32);
        assert_eq!(ordinary.as_ref(), bounded.as_slice());
    }

    let header = Header::fixtures().into_iter().next().unwrap().value;
    let ordinary = header.to_bytes().unwrap();
    let bounded = header.to_bounded_bytes().unwrap();
    assert_eq!(ordinary.len(), HEADER_BINCODE_SIZE);
    assert_eq!(bounded.as_ref(), ordinary.as_ref());
}

#[test]
fn fixed_header_byte_types_borrow_their_stored_bytes() {
    let content_id = ContentId([0x22; 32]);
    let nonce = Nonce([0x33; 32]);

    assert_eq!(content_id.as_ref(), &content_id.0);
    assert_eq!(nonce.as_ref(), &nonce.0);
    assert!(std::ptr::eq(content_id.as_ref(), &raw const content_id.0));
    assert!(std::ptr::eq(nonce.as_ref(), &raw const nonce.0));
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

#[test]
fn pol_msg_hashes_the_header_fields_under_its_dst() {
    use std::str::FromStr as _;

    let parent_block = HeaderId([0x11; 32]);
    let body_root = ContentId(
        hex::decode("74e48ccd43051016138065265ee8af74cdd43b97c1624dd463da1fb9b345dc3f")
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let reward_commitment = NoteCommitment(Fr::from(0x4444u64));

    // Digest below p.
    assert_eq!(
        pol_msg(
            &parent_block,
            Slot::from(46u64),
            &body_root,
            &reward_commitment,
            0x6666
        ),
        Fr::from_str(
            "1257913445352186760781801521745335656558978515391203688562859797786293976798"
        )
        .unwrap()
    );
    // Digest 0x69353504...6c706f54 read little endian is above p and wraps.
    assert_eq!(
        pol_msg(
            &parent_block,
            Slot::from(42u64),
            &body_root,
            &reward_commitment,
            0x6666
        ),
        Fr::from_str(
            "16302932358473993454067199317569404181679737752948030160294528723108679202152"
        )
        .unwrap()
    );
    // The reward commitment and the reward value are covered.
    assert_eq!(
        pol_msg(
            &parent_block,
            Slot::from(42u64),
            &body_root,
            &NoteCommitment(Fr::from(0x4445u64)),
            0x6666
        ),
        Fr::from_str(
            "6675858732187783999506042311682083436339216882710970592929130357297914894510"
        )
        .unwrap()
    );
    assert_eq!(
        pol_msg(
            &parent_block,
            Slot::from(42u64),
            &body_root,
            &reward_commitment,
            0x6667
        ),
        Fr::from_str(
            "19127765436389383361965734721894489407953894745175146900948303423486380591301"
        )
        .unwrap()
    );
}

#[test]
fn block_id_matches_the_spec_vector_and_leaves_out_the_proof() {
    let body_root = ContentId(
        hex::decode("521a7a43fa773b034c76c54eab7b45f5264cce10944eeb8b47606d6448418e41")
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let header = |proof_byte: u8| {
        Header::new(
            HeaderId([0x11; 32]),
            body_root,
            Slot::from(42u64),
            Groth16LeaderProof::from_parts(
                lb_pol::PoLProof::from_bytes(&[proof_byte; 128]),
                Fr::from(0x5555u64),
                NoteCommitment(Fr::from(0x4444u64)),
                0x6666,
            ),
        )
    };
    assert_eq!(
        hex::encode(header(0x22).id().0),
        "6b07ccc869a11a4e4974b24b90598711ceafffe544ed910d124372fcf5da8199"
    );
    assert_eq!(header(0x22).id(), header(0x23).id());
    assert_eq!(
        header(0x22).pol_msg(),
        pol_msg(
            &HeaderId([0x11; 32]),
            Slot::from(42u64),
            &body_root,
            &NoteCommitment(Fr::from(0x4444u64)),
            0x6666
        )
    );

    // The current spec vector, over the `body_root` of its empty uncle list.
    let spec_header = Header::new(
        HeaderId([0x11; 32]),
        ContentId(
            hex::decode("521a7a43fa773b034c76c54eab7b45f5264cce10944eeb8b47606d6448418e41")
                .unwrap()
                .try_into()
                .unwrap(),
        ),
        Slot::from(42u64),
        Groth16LeaderProof::from_parts(
            lb_pol::PoLProof::from_bytes(&[0x22; 128]),
            Fr::from(0x5555u64),
            NoteCommitment(Fr::from(0x4444u64)),
            0x6666,
        ),
    );
    assert_eq!(
        hex::encode(spec_header.id().0),
        "6b07ccc869a11a4e4974b24b90598711ceafffe544ed910d124372fcf5da8199"
    );
}

#[test]
fn header_proof_verifies_only_against_its_own_pol_msg() {
    let parent_block = HeaderId([0x11; 32]);
    let body_root = ContentId([0x22; 32]);
    // The witness helper proves at slot 0.
    let slot = Slot::from(0u64);
    let reward = crate::mantle::Note::new(
        0x66,
        Fr::from(0x55u64),
        lb_key_management_system_keys::keys::ZkPublicKey::new(Fr::from(0x44u64)),
    );
    let msg = pol_msg(
        &parent_block,
        slot,
        &body_root,
        &reward.commitment(),
        reward.value,
    );
    let (public, private) = crate::proofs::leader_proof::test_utils::note_set_leader_inputs(
        Fr::from(7u64),
        1000,
        Fr::from(11u64),
        msg,
        &reward,
    );
    let proof = Groth16LeaderProof::prove(private).unwrap();

    assert!(Header::new(parent_block, body_root, slot, proof.clone()).verify(&public));
    assert!(!Header::new(parent_block, ContentId([0x23; 32]), slot, proof.clone()).verify(&public));
    let redirected = Groth16LeaderProof::from_parts(
        *proof.proof(),
        proof.entropy(),
        NoteCommitment(Fr::from(0x45u64)),
        reward.value,
    );
    assert!(!Header::new(parent_block, body_root, slot, redirected).verify(&public));
    let overpaid = Groth16LeaderProof::from_parts(
        *proof.proof(),
        proof.entropy(),
        reward.commitment(),
        reward.value + 1,
    );
    assert!(!Header::new(parent_block, body_root, slot, overpaid).verify(&public));
}

#[test]
fn header_wire_starts_with_the_bedrock_version() {
    use lb_binary_codec::canonical::CodecExamples as _;

    let header = Header::fixtures().into_iter().next().unwrap().value;
    let mut bytes = header.encode_to_vec();
    assert_eq!(bytes.len(), Header::CANONICAL_ENCODED_SIZE);
    assert_eq!(bytes[0], BEDROCK_VERSION);
    assert_eq!(Header::decode_all(&bytes, &()).unwrap(), header);

    bytes[0] = BEDROCK_VERSION + 1;
    assert!(Header::decode_all(&bytes, &()).is_err());
}

#[test]
fn header_with_a_version_byte_other_than_one_is_rejected() {
    use lb_binary_codec::canonical::CodecExamples as _;

    assert_eq!(BEDROCK_VERSION, 0x01);
    let header = Header::fixtures().into_iter().next().unwrap().value;
    let mut bytes = header.encode_to_vec();
    for version in (0..=u8::MAX).filter(|version| *version != 0x01) {
        bytes[0] = version;
        assert!(
            Header::decode_all(&bytes, &()).is_err(),
            "version {version:#04x}"
        );
    }
}

#[test]
fn peek_slot_reads_the_slot_after_the_version_and_the_parent() {
    use lb_binary_codec::canonical::CodecExamples as _;

    let header = Header::fixtures().into_iter().next().unwrap().value;
    assert_eq!(header.slot(), Slot::from(42u64));
    let bytes = header.encode_to_vec();

    assert_eq!(Header::peek_slot(&bytes), Ok(Slot::from(42u64)));
    assert!(Header::peek_slot(&bytes[..33]).is_err());
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
/// little-endian element count followed by that many fixed 273-byte entries.
///
/// `HeaderId (block_id) = blake2b256( b"BLOCK_ID_V1" || bedrock_version (1B)
/// ||` `parent_block (32B) || slot_le (8B) || body_root (32B) ||
/// reward_commitment (32B) || reward_value_le (8B) ||` `entropy_contribution
/// (32B) )`. The proof is left out, and the preimage absorbs
/// `reward_commitment` and `reward_value` before `entropy_contribution`, which
/// is not the header's wire order.
///
/// The test is `#[ignore]`d so it is skipped by `cargo test --all-features`.
/// Run it on demand with:
/// `cargo test -p logos-blockchain-core body_root_test_vectors -- --ignored
/// --nocapture`
#[cfg(test)]
mod body_root_test_vectors {
    use super::*;
    use crate::{
        block::UncleHeaders,
        mantle::{traits::Hashable as _, transactions::Ops},
        utils::merkle,
    };

    /// The uncle maximum of the vectors' era.
    const VECTOR_MAX_UNCLES: u8 = 4;

    fn uncle(byte: u8) -> Header {
        Header::new(
            HeaderId([byte; 32]),
            ContentId([byte; 32]),
            Slot::from(u64::from(byte)),
            Groth16LeaderProof::from_parts(
                // `[1]G1`, `[1]G2`, `[1]G1`, real points whose flags are clear.
                lb_pol::PoLProof::try_from_bytes(
                    &hex::decode(concat!(
                        "0100000000000000000000000000000000000000000000000000000000000000",
                        "edf692d95cbdde46ddda5ef7d422436779445c5e66006a42761e1f12efde0018",
                        "c212f3aeb785e49712e7a9353349aaf1255dfb31b7bf60723a480d9293938e19",
                        "0100000000000000000000000000000000000000000000000000000000000000",
                    ))
                    .unwrap()
                    .try_into()
                    .unwrap(),
                )
                .unwrap(),
                Fr::from(u64::from(byte)),
                NoteCommitment(Fr::from(u64::from(byte))),
                u64::from(byte),
            ),
        )
    }

    /// Generates (and prints) the `body_root` / `HeaderId` test vectors.
    /// Ignored by default so it never runs under `cargo test --all-features`;
    /// invoke explicitly with `--ignored --nocapture` to regenerate the
    /// vectors.
    #[test]
    #[ignore = "generates body_root/HeaderId test vectors on demand; run with --ignored --nocapture"]
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
             slot_le || body_root || reward_commitment || reward_value_le || \
             entropy_contribution )"
        );

        // 1. Empty block: no transactions.
        let empty: Vec<Ops> = vec![];
        println!("================================================================");
        println!("vector 1  : empty block (0 transactions)");
        println!(
            "{:20}: {}",
            "transactions_root",
            hex::encode(merkle::calculate_transactions_root(&empty))
        );

        // 2. One transaction per operation kind (one op each), labelled by that
        //    operation's name.
        let txs_with_names: Vec<(&'static str, Ops)> = Ops::sample()
            .into_iter()
            .map(|op| (op.as_str(), Ops::from([op])))
            .collect();
        let txs: Vec<Ops> = txs_with_names.iter().map(|(_, tx)| tx.clone()).collect();
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
        //    `uncle_headers` encoding.
        let uncles =
            UncleHeaders::try_new(vec![uncle(0x66), uncle(0x77)], VECTOR_MAX_UNCLES).unwrap();
        println!("================================================================");
        println!("vector 4  : body_root with 2 uncles, over vector 2's transactions");
        println!("{:20}: {:02x}", "uncle_count", uncles.len());
        for (index, uncle) in uncles.iter().enumerate() {
            println!(
                "{:20}: {}",
                format!("uncle_headers[{index}]"),
                hex::encode(uncle.encode_to_vec())
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
            Fr::from(0x5555u64),                 // entropy_contribution
            NoteCommitment(Fr::from(0x4444u64)), // reward_commitment
            0x6666,                              // reward_value
        );
        let header = Header::new(parent_block, body_root, slot, proof);
        let header_id = header.id();

        // Self-check: recompute the preimage from the public accessors and make
        // sure it matches `Header::id()` (the exact byte layout is documented in
        // the module-level comment).
        let proof = header.leader_proof();
        let mut h = Hasher::new();
        h.update(b"BLOCK_ID_V1");
        h.update([BEDROCK_VERSION]);
        h.update(parent_block.0);
        h.update(slot.to_le_bytes());
        h.update(body_root.0);
        h.update(fr_to_bytes(&proof.reward_commitment().0));
        h.update(proof.reward_value().to_le_bytes());
        h.update(fr_to_bytes(&proof.entropy()));
        let manual: [u8; 32] = h.finalize().into();
        assert_eq!(
            manual, header_id.0,
            "manual preimage must match Header::id()"
        );

        println!("================================================================");
        // Field labels match the names in the `block_id`/`Header` specification.
        println!("vector 5  : HeaderId (block_id) reusing vector 3's body_root");
        println!("{:20}: {:02x}", "bedrock_version", BEDROCK_VERSION);
        println!("{:20}: {}", "parent_block", hex::encode(parent_block.0));
        println!("{:20}: {}", "slot", u64::from(slot));
        println!("{:20}: {}", "body_root", hex::encode(body_root.0));
        // proof_of_leadership fields (here, the deterministic genesis proof).
        let reward_commitment = hex::encode(fr_to_bytes(&proof.reward_commitment().0));
        println!("{:20}: {reward_commitment}", "reward_commitment");
        println!("{:20}: {}", "reward_value", proof.reward_value());
        let entropy_contribution = hex::encode(fr_to_bytes(&proof.entropy()));
        println!("{:20}: {entropy_contribution}", "entropy_contribution");
        println!("{:20}: {}", "block_id", hex::encode(header_id.0));
        println!("================================================================");
    }

    #[test]
    fn body_root_matches_the_spec_vectors() {
        let txs: Vec<Ops> = Ops::sample()
            .into_iter()
            .map(|op| Ops::from([op]))
            .collect();
        assert_eq!(
            hex::encode(merkle::calculate_transactions_root(&txs)),
            "0815ddf6cc5e2090d5f7088f6d5255d79f3ee0026a296aa3cc9acfc092741e88"
        );
        assert_eq!(
            hex::encode(crate::block::body_root(&UncleHeaders::empty(), &txs).0),
            "521a7a43fa773b034c76c54eab7b45f5264cce10944eeb8b47606d6448418e41"
        );
        let uncles =
            UncleHeaders::try_new(vec![uncle(0x66), uncle(0x77)], VECTOR_MAX_UNCLES).unwrap();
        assert_eq!(
            hex::encode(crate::block::body_root(&uncles, &txs).0),
            "68ef845dd10541fc58f90c0fe3f347bea8ac1a0bbc5a87f2bd0c7552993c54bb"
        );
    }
}
