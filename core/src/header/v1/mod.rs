//! The header of the blocks of version 1.

use blake2::Digest as _;
use lb_binary_codec::{
    bincode::{BoundedSerializeOp, SerializeOp as _},
    canonical::{BinaryDecode, BinaryEncode, DecodeError},
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};
use serde::{Deserialize, Serialize};

mod fixtures;

use crate::{
    crypto::Hasher,
    header::{ContentId, HeaderId},
    mantle::transactions::GenesisTx,
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof as _},
};

pub const HEADER_BINCODE_SIZE: usize = <Slot as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + <HeaderId as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + <ContentId as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + <Groth16LeaderProof as BoundedSerializeOp>::MAX_ENCODED_SIZE;

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

        slot.encoded_length()
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

        // The first field, which must never change across eras. A node reads the slot
        // to learn which era's rules parse the rest of the header.
        slot.encode_into(out);
        parent_block.encode_into(out);
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
        let (input, slot) = Slot::decode(input, context)?;
        let (input, parent_block) = HeaderId::decode(input, context)?;
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
    pub const CANONICAL_ENCODED_SIZE: usize = Slot::CANONICAL_ENCODED_SIZE
        + HeaderId::CANONICAL_ENCODED_SIZE
        + ContentId::CANONICAL_ENCODED_SIZE
        + Groth16LeaderProof::CANONICAL_ENCODED_SIZE;

    #[must_use]
    pub const fn parent(&self) -> HeaderId {
        self.parent_block
    }

    fn update_hasher(&self, h: &mut Hasher) {
        h.update(b"BLOCK_ID_V1");
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
            crate::block::v1::body_root(&crate::block::v1::UncleHeaders::empty(), &[tx]),
            Slot::from(0u64),
            Groth16LeaderProof::genesis(),
        )
    }
}

impl BoundedSerializeOp for Header {
    type Bytes = [u8; HEADER_BINCODE_SIZE];
}

#[test]
fn fixed_size_bincode_serialization_matches_for_the_header() {
    use lb_binary_codec::canonical::CodecExamples as _;

    let header = Header::fixtures().into_iter().next().unwrap().value;
    let ordinary = header.to_bytes().unwrap();
    let bounded = header.to_bounded_bytes().unwrap();
    assert_eq!(ordinary.len(), HEADER_BINCODE_SIZE);
    assert_eq!(bounded.as_ref(), ordinary.as_ref());
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
/// little-endian element count followed by that many fixed 360-byte entries.
///
/// `HeaderId (block_id) = blake2b256( b"BLOCK_ID_V1" || parent_block (32B) ||`
/// `slot_le (8B) || body_root (32B) || leader_voucher (32B) ||`
/// `entropy_contribution (32B) || proof (128B) || leader_key (32B) )`. The
/// preimage keeps this field order, which is not the header's wire order
/// (`slot` leads there).
///
/// The test is `#[ignore]`d so it is skipped by `cargo test --all-features`.
/// Run it on demand with:
/// `cargo test -p logos-blockchain-core body_root_test_vectors -- --ignored
/// --nocapture`
#[cfg(test)]
mod body_root_test_vectors {
    use lb_poseidon2::Fr;

    use super::*;
    use crate::{
        block::v1::{SignedHeader, UncleHeaders},
        mantle::{ops::leader_claim::VoucherCm, traits::Hashable as _, transactions::Ops},
        utils::merkle,
    };

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
            "block_id   = blake2b256( b\"BLOCK_ID_V1\" || parent_block || slot_le || body_root \
             || leader_voucher || entropy_contribution || proof || leader_key )"
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
        let body_root = crate::block::v1::body_root(&UncleHeaders::empty(), &txs);
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
            hex::encode(crate::block::v1::body_root(&uncles, &txs).0)
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
