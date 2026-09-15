pub mod channel;
pub(crate) mod internal;
pub mod leader_claim;
pub mod op;
pub mod op_proof;
pub mod op_proof_ref;
pub mod op_ref;
pub mod pow;
pub mod proof_noop;
pub mod proof_zk_and_ed;
pub mod sdp;
mod serde_;
pub mod signed_op;
pub mod signed_op_error;
pub mod signed_operation;
pub mod transfer;

use std::sync::LazyLock;

pub use crate::mantle::ops::{
    op::{Op, OpId},
    op_proof::OpProof,
    op_proof_ref::OpProofRef,
    op_ref::OpRef,
    proof_noop::NoOpProof,
    proof_zk_and_ed::ZkAndEd25519Proof,
    signed_op::SignedOp,
    signed_operation::SignedOperation,
};

pub(crate) static OPERATION_ID_V1: LazyLock<Vec<u8>> =
    LazyLock::new(|| b"OPERATION_ID_V1".to_vec());

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
    use lb_codec::BinaryEncode as _;

    use super::*;
    use crate::{
        crypto::{Digest as _, Hasher},
        mantle::{traits::Hashable as _, transactions::tx_list::Ops},
    };

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

    fn print_tx_vector(label: &str, tx: &Ops) {
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
        for op in &Ops::sample() {
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
        print_tx_vector("empty (0 ops)", &Ops::new_unchecked(vec![]));

        // Transaction holding one of every operation.
        print_tx_vector("one of each operation (11 ops)", &Ops::sample());
    }
}
