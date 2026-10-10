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
///   implement [`OpId`] (`Transfer`, `ChannelDeposit`, `ChannelTransfer`,
///   `ChannelWithdraw`, `SDPWithdraw`, `LeaderClaim`) the emitted `op_id` is
///   asserted to equal `OpId::op_id`.
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
    use lb_binary_codec::canonical::BinaryEncode as _;

    use super::*;
    use crate::{
        crypto::{Digest as _, Hasher},
        mantle::{
            fixtures::ops::op_values::{
                ALL_OPS_COLUMN_HEX, CHANNEL_CONFIG, CHANNEL_TRANSFER, CHANNEL_WITHDRAW,
                CLAIM_POW_REWARD, DEPOSIT, INSCRIPTION, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE,
                SDP_WITHDRAW, SDP_WITHDRAW_PAYLOAD_HEX, TRANSFER,
            },
            traits::Hashable as _,
            transactions::tx_list::Ops,
        },
        sdp::{DeclarationId, Nonce, WithdrawMessage},
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

    /// The repository-maintained fixture uses declaration ID 0x70… and nonce
    /// 0x72, and its all-operation transaction contains all eleven variants.
    /// These are implementation fixtures, separate from the RFC withdrawal
    /// vector's 0x1b/0x1d values.
    #[test]
    fn maintained_sdp_withdraw_and_all_operations_vectors() {
        let withdraw = Op::SDPWithdraw(*SDP_WITHDRAW);
        let withdraw_encoding = withdraw.encode();
        let withdraw_payload = &withdraw_encoding[1..];
        let withdraw_id = SDP_WITHDRAW.op_id();
        assert_eq!(withdraw_payload.len(), 40);
        assert_eq!(hex::encode(withdraw_payload), SDP_WITHDRAW_PAYLOAD_HEX);
        assert_eq!(withdraw_id, op_id_from_payload(withdraw_payload));
        assert_eq!(
            hex::encode(withdraw_id),
            "e6d384d4b0efa9e934c08953d225752b91b202f0d085f67e9905343fe74808fa"
        );

        let all_ops = Ops::from([
            Op::Transfer(TRANSFER.clone()),
            Op::ChannelConfig(CHANNEL_CONFIG.clone()),
            Op::ChannelInscribe(INSCRIPTION.clone()),
            Op::ChannelDeposit(DEPOSIT.clone()),
            Op::ChannelWithdraw(CHANNEL_WITHDRAW.clone()),
            Op::ChannelTransfer(CHANNEL_TRANSFER.clone()),
            Op::SDPDeclare(SDP_DECLARE.clone()),
            Op::SDPWithdraw(*SDP_WITHDRAW),
            Op::SDPActive(SDP_ACTIVE.clone()),
            Op::LeaderClaim(LEADER_CLAIM.clone()),
            Op::ClaimPowReward(CLAIM_POW_REWARD.clone()),
        ]);
        let transaction_encoding = all_ops.encode();
        let transaction_hash = tx_hash_from_payload(&transaction_encoding);
        assert_eq!(transaction_encoding[0], 0x0b);
        assert_eq!(hex::encode(&transaction_encoding), ALL_OPS_COLUMN_HEX);
        assert_eq!(all_ops.hash().0, transaction_hash);
        assert_eq!(
            hex::encode(transaction_hash),
            "2b6cc712040538e08efef92be1435b38f4f2e2aa7ca5677606b9b2a71238b89f"
        );
    }

    /// RFC #407's withdrawal row uses ID [0x1b; 32] and nonce 0x1d. Keep its
    /// payload and operation ID separate from the repository's 0x70/0x72
    /// implementation fixture.
    #[test]
    fn rfc_sdp_withdraw_operation_vector() {
        let withdraw = WithdrawMessage {
            declaration_id: DeclarationId([0x1b; 32]),
            nonce: Nonce::new(0.into(), 0x1d),
        };
        let payload = withdraw.encode_to_vec();
        let expected_payload = [vec![0x1b; 32], 0x1du64.to_le_bytes().to_vec()].concat();

        assert_eq!(payload.len(), 40);
        assert_eq!(payload, expected_payload);
        assert_eq!(
            hex::encode(&payload),
            format!("{}1d00000000000000", "1b".repeat(32))
        );
        assert_eq!(
            hex::encode(withdraw.op_id()),
            "a9f938ca71aa93a05f19aff553757b2707041daf0630b7c2d6d3da0705be8aaa"
        );
    }

    /// The current one-of-each transaction contains all eleven operation
    /// variants, including `CLAIM_POW_REWARD`.
    #[test]
    fn one_of_each_transaction_vector_includes_claim_pow_reward() {
        let current_ops = Ops::sample();
        let encoding = current_ops.encode();
        let hash = current_ops.hash().0;

        assert_eq!(encoding[0], 0x0b);
        assert!(matches!(
            current_ops.as_slice(),
            [
                Op::Transfer(_),
                Op::ChannelConfig(_),
                Op::ChannelInscribe(_),
                Op::ChannelDeposit(_),
                Op::ChannelWithdraw(_),
                Op::ChannelTransfer(_),
                Op::SDPDeclare(_),
                Op::SDPWithdraw(_),
                Op::SDPActive(_),
                Op::LeaderClaim(_),
                Op::ClaimPowReward(_),
            ]
        ));
        assert_eq!(hash, tx_hash_from_payload(&encoding));
        assert_eq!(
            hex::encode(hash),
            "fa9bb4104085f58ecf14fe698f72c1eb1de893c239bfdcf01d308590e56b341c"
        );
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
                Op::SDPWithdraw(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
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
