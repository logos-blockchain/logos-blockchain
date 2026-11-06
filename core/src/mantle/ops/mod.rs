pub mod channel;
pub(crate) mod internal;
pub mod op;
pub mod op_proof;
pub mod op_proof_ref;
pub mod op_ref;
pub mod pow;
pub mod program;
pub mod proof_channel_inscribe;
pub mod proof_noop;
pub mod proof_program_step;
pub mod proof_zk_and_ed;
pub mod proof_zk_transfer;
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
    proof_channel_inscribe::ChannelInscribeProof,
    proof_noop::NoOpProof,
    proof_program_step::{
        ProgramNoteOpening, ProgramProof, ProgramStepProof, StepProof, StepProofShape,
        ZkProgramSpendProof,
    },
    proof_zk_and_ed::ZkAndEd25519Proof,
    proof_zk_transfer::ZkTransferProof,
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
///   `SDPWithdraw`) the emitted `op_id` is asserted to equal `OpId::op_id`.
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
                ALL_OPS_COLUMN_HEX, CHANNEL_CONFIG, CHANNEL_WITHDRAW, CLAIM_POW_REWARD, DEPOSIT,
                INSCRIPTION, SDP_ACTIVE, SDP_DECLARE, SDP_WITHDRAW, SDP_WITHDRAW_PAYLOAD_HEX,
                TRANSFER,
            },
            ledger::ProvableOperation as _,
            traits::Hashable as _,
            transactions::tx_list::Ops,
        },
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

    /// The repository-maintained fixture uses declaration ID 0x1b… and nonce
    /// 0x1d, and its all-operation transaction contains one of each variant.
    #[test]
    fn maintained_sdp_withdraw_and_all_operations_vectors() {
        let withdraw = Op::SDPWithdraw(SDP_WITHDRAW.clone());
        let withdraw_encoding = withdraw.encode();
        let withdraw_payload = &withdraw_encoding[1..];
        let withdraw_id = SDP_WITHDRAW.op_id();
        assert_eq!(hex::encode(withdraw_payload), SDP_WITHDRAW_PAYLOAD_HEX);
        assert_eq!(withdraw_id, op_id_from_payload(withdraw_payload));
        assert_eq!(
            hex::encode(withdraw_id),
            "f385d531e2efc2cfca261d1b12643937a3ba2d19a6f7be9b617d59e2d3264f8a"
        );

        let all_ops = Ops::from([
            Op::Transfer(TRANSFER.clone()),
            Op::ChannelConfig(CHANNEL_CONFIG.clone()),
            Op::ChannelInscribe(INSCRIPTION.clone()),
            Op::ChannelDeposit(DEPOSIT.clone()),
            Op::ChannelWithdraw(CHANNEL_WITHDRAW.clone()),
            Op::SDPDeclare(SDP_DECLARE.clone()),
            Op::SDPWithdraw(SDP_WITHDRAW.clone()),
            Op::SDPActive(SDP_ACTIVE.clone()),
            Op::ClaimPowReward(CLAIM_POW_REWARD.clone()),
        ]);
        let transaction_encoding = all_ops.encode();
        let transaction_hash = tx_hash_from_payload(&transaction_encoding);
        assert_eq!(transaction_encoding[0], 0x09);
        assert_eq!(hex::encode(&transaction_encoding), ALL_OPS_COLUMN_HEX);
        assert_eq!(all_ops.hash().0, transaction_hash);
        assert_eq!(
            hex::encode(transaction_hash),
            "3ef54a276aea3acb61ae4c26b0cba9fb7f8e5508b81453eee56b72dc10ad42b3"
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
                Op::ChannelWithdraw(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
                Op::SDPWithdraw(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
                Op::ProgramRegister(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
                Op::ChannelInscribe(o) => assert_eq!(o.op_id(), op_id_from_payload(&o.op_bytes())),
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
        print_tx_vector("one of each operation (10 ops)", &Ops::sample());
    }

    #[test]
    fn transfer_op_id_matches_the_spec_vector() {
        let transfer = transfer::TransferOp::sample();
        assert_eq!(
            hex::encode(transfer.op_bytes()),
            "0201000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000020300000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000005000000000000000000000000000000000000000000000000000000000000000600000000000000"
        );
        assert_eq!(
            hex::encode(transfer.op_id()),
            "b39493d223a2fefae67c6aac3c8f9338e3272137d4ae4ce714607bcbc9d011a4"
        );
    }

    #[test]
    fn claim_pow_reward_op_id_matches_the_spec_vector() {
        let claim = pow::ClaimPowRewardOp::sample();
        assert_eq!(
            hex::encode(claim.op_bytes()),
            "3000000000000000000000000000000000000000000000000000000000000000313131313131313131313131313131313131313131313131313131313131313132000000000000000000000000000000000000000000000000000000000000003300000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(claim.op_id()),
            "bc5ccb3bf2ceae92f0479fb84cd1e8049d4e7cb33cc72d16a3d5e30780a34c2b"
        );
        assert_eq!(pow::ClaimPowRewardOp::CODE, 0x30);
    }

    #[test]
    fn program_register_op_id_matches_the_spec_vector() {
        let register = Op::ProgramRegister(program::ProgramRegisterOp::sample());
        let payload = &register.encode()[1..];
        assert_eq!(
            hex::encode(payload),
            "0100000000000000000000000000000000000000000000000000000000000000b9b3b4620913f849ee2aa6a9cfd35c9d146f3e7c27596cc3e8d311fd3472dc2779ad28398ced57998435d8c63164b86d7033733ab82101b6379bf1b45d203ea0f5c7fb9406fade12005e9ab08c477e8d5e7192e12628e551900eb14d784e0606856eb6ff324f82c9a7a078686b1504bcebe4cf5dcd9151734297bb572f7714102ce236b4ddb40977fd454c08e570151342c4cc50a86380c3472f6a6ff8dbdd296f237195c2856d5a3f8ca190f6666f2084d776f9935a48a2d57a569e476806a9a93f16faa7a849e89ca35389d8dee46243772b760402bc66f7e0fe0edf39c117f94a688c5b3dfe2a762ab3d315927156987792e7d92f79e0f4523f1a41caf40978e0ffab866b3a9876bd01b8ecc66fcb86936277f425539a758dbbd32e2b0717636a302c88373904f75a15fe21c13882a8fe9c3be9f572f4100c48231dd5b188"
        );
        assert_eq!(
            hex::encode(op_id_from_payload(payload)),
            "87e8c6cec157aab8a5f80f9058851063f8f4586992bcb54a256769694cbb0dd1"
        );
    }

    #[test]
    fn channel_deposit_op_id_matches_the_spec_vector() {
        let deposit = channel::deposit::DepositOp::sample();
        assert_eq!(
            hex::encode(deposit.op_bytes()),
            "101010101010101010101010101010101010101010101010101010101010101001110000000000000000000000000000000000000000000000000000000000000026000000000000000000000000000000000000000000000000000000000000000127000000000000000000000000000000000000000000000000000000000000002800000000000000100000006465706f7369742d6d65746164617461"
        );
        assert_eq!(
            hex::encode(deposit.op_id()),
            "83eeb0ac0b7e03bb6c93c0c7596512413707b9536b2b3d56a86656f8f527d6f6"
        );
    }

    #[test]
    fn channel_withdraw_op_id_matches_the_spec_vector() {
        let withdraw = channel::withdraw::ChannelWithdrawOp::sample();
        assert_eq!(
            hex::encode(withdraw.op_bytes()),
            "12121212121212121212121212121212121212121212121212121212121212120113000000000000000000000000000000000000000000000000000000000000002900000000000000000000000000000000000000000000000000000000000000012a00000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(withdraw.op_id()),
            "8d6ae69200aab259bca71e5829d43f0a289ba612383d29ddcd7d217f1ede0996"
        );
    }

    #[test]
    fn channel_config_op_id_matches_the_spec_vector() {
        let config = Op::ChannelConfig(channel::config::ChannelConfigOp::sample());
        let payload = &config.encode()[1..];
        assert_eq!(
            hex::encode(payload),
            "0707070707070707070707070707070707070707070707070707070707070707000000000000000000000000000000000000000000000000000000000000000002001398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93cafd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f6180a0000000b0000000200"
        );
        assert_eq!(
            hex::encode(op_id_from_payload(payload)),
            "c9f038b929299dd9ef28856916375021b04ce20e284bbfdc4355edab718846c2"
        );
    }

    #[test]
    fn channel_inscribe_op_id_matches_the_spec_vector() {
        let inscribe = Op::ChannelInscribe(channel::inscribe::InscriptionOp::sample_with_step());
        let payload = &inscribe.encode()[1..];
        assert_eq!(
            hex::encode(payload),
            "0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0b00000068656c6c6f206c6f676f730000000000000000000000000000000000000000000000000000000000000000d9bf2148748a85c89da5aad8ee0b0fc2d105fd39d41a4c796536354f0ae2900c0100020000012300000000000000000000000000000000000000000000000000000000000000012400000000000000000000000000000000000000000000000000000000000000250000000000000000000000000000000000000000000000000000000000000010000000737465702d696e736372697074696f6e01340000000000000000000000000000000000000000000000000000000000000001013500000000000000000000000000000000000000000000000000000000000000013600000000000000000000000000000000000000000000000000000000000000370000000000000000000000000000000000000000000000000000000000000038000000000000003900000000000000000000000000000000000000000000000000000000000000130000007061796d656e742d696e736372697074696f6e013a00000000000000000000000000000000000000000000000000000000000000013b00000000000000000000000000000000000000000000000000000000000000013c000000000000000000000000000000000000000000000000000000000000003d000000000000000000000000000000000000000000000000000000000000001800000070726f6772616d2d737465702d696e736372697074696f6e"
        );
        assert_eq!(
            hex::encode(op_id_from_payload(payload)),
            "b8f9719ad3893f7909960208f665a76b29424de7180bdf524741367b2c56bc47"
        );
        if let Op::ChannelInscribe(operation) = &inscribe {
            assert_eq!(
                hex::encode(operation.op_id()),
                "b8f9719ad3893f7909960208f665a76b29424de7180bdf524741367b2c56bc47"
            );
        }
    }

    #[test]
    fn sdp_declare_op_id_matches_the_spec_vector() {
        let declare = sdp::SDPDeclareOp::sample();
        assert_eq!(
            hex::encode(declare.op_bytes()),
            "00010b00047f00000191020bb8cd0353470962558a6e0839022ae65c6b2723b32772e5c0c5f4776cb8e6a3e10ba2f319000000000000000000000000000000000000000000000000000000000000003000000000000000000000000000000000000000000000000000000000000000011a000000000000000000000000000000000000000000000000000000000000002c000000000000000000000000000000000000000000000000000000000000002d00000000000000"
        );
        assert_eq!(
            hex::encode(declare.op_id()),
            "07cad981e914c28fb8d090e67627b05347314ad27090a7212cd3437ba66db105"
        );
    }

    #[test]
    fn sdp_withdraw_payload_matches_the_spec_vector() {
        let payload = sdp::SDPWithdrawOp::sample().encode();
        assert_eq!(
            hex::encode(&payload),
            "1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1d000000000000001c00000000000000000000000000000000000000000000000000000000000000012e000000000000000000000000000000000000000000000000000000000000002f00000000000000"
        );
        assert_eq!(
            hex::encode(op_id_from_payload(&payload)),
            "f385d531e2efc2cfca261d1b12643937a3ba2d19a6f7be9b617d59e2d3264f8a"
        );
    }

    #[test]
    fn one_of_each_operation_hash_matches_the_spec_vector() {
        let tx = Ops::sample();
        assert_eq!(
            hex::encode(tx.encode()),
            "0a000201000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000020300000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000005000000000000000000000000000000000000000000000000000000000000000600000000000000010100000000000000000000000000000000000000000000000000000000000000b9b3b4620913f849ee2aa6a9cfd35c9d146f3e7c27596cc3e8d311fd3472dc2779ad28398ced57998435d8c63164b86d7033733ab82101b6379bf1b45d203ea0f5c7fb9406fade12005e9ab08c477e8d5e7192e12628e551900eb14d784e0606856eb6ff324f82c9a7a078686b1504bcebe4cf5dcd9151734297bb572f7714102ce236b4ddb40977fd454c08e570151342c4cc50a86380c3472f6a6ff8dbdd296f237195c2856d5a3f8ca190f6666f2084d776f9935a48a2d57a569e476806a9a93f16faa7a849e89ca35389d8dee46243772b760402bc66f7e0fe0edf39c117f94a688c5b3dfe2a762ab3d315927156987792e7d92f79e0f4523f1a41caf40978e0ffab866b3a9876bd01b8ecc66fcb86936277f425539a758dbbd32e2b0717636a302c88373904f75a15fe21c13882a8fe9c3be9f572f4100c48231dd5b188100707070707070707070707070707070707070707070707070707070707070707000000000000000000000000000000000000000000000000000000000000000002001398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93cafd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f6180a0000000b0000000200110e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0b00000068656c6c6f206c6f676f730000000000000000000000000000000000000000000000000000000000000000d9bf2148748a85c89da5aad8ee0b0fc2d105fd39d41a4c796536354f0ae2900c0100020000012300000000000000000000000000000000000000000000000000000000000000012400000000000000000000000000000000000000000000000000000000000000250000000000000000000000000000000000000000000000000000000000000010000000737465702d696e736372697074696f6e01340000000000000000000000000000000000000000000000000000000000000001013500000000000000000000000000000000000000000000000000000000000000013600000000000000000000000000000000000000000000000000000000000000370000000000000000000000000000000000000000000000000000000000000038000000000000003900000000000000000000000000000000000000000000000000000000000000130000007061796d656e742d696e736372697074696f6e013a00000000000000000000000000000000000000000000000000000000000000013b00000000000000000000000000000000000000000000000000000000000000013c000000000000000000000000000000000000000000000000000000000000003d000000000000000000000000000000000000000000000000000000000000001800000070726f6772616d2d737465702d696e736372697074696f6e12101010101010101010101010101010101010101010101010101010101010101001110000000000000000000000000000000000000000000000000000000000000026000000000000000000000000000000000000000000000000000000000000000127000000000000000000000000000000000000000000000000000000000000002800000000000000100000006465706f7369742d6d657461646174611312121212121212121212121212121212121212121212121212121212121212120113000000000000000000000000000000000000000000000000000000000000002900000000000000000000000000000000000000000000000000000000000000012a000000000000000000000000000000000000000000000000000000000000002000010b00047f00000191020bb8cd0353470962558a6e0839022ae65c6b2723b32772e5c0c5f4776cb8e6a3e10ba2f319000000000000000000000000000000000000000000000000000000000000003000000000000000000000000000000000000000000000000000000000000000011a000000000000000000000000000000000000000000000000000000000000002c000000000000000000000000000000000000000000000000000000000000002d00000000000000211b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1d000000000000001c00000000000000000000000000000000000000000000000000000000000000012e000000000000000000000000000000000000000000000000000000000000002f00000000000000221e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1f00000000000000010a0000008a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020303030303030303030303030303030303030303030303030303030303030303303000000000000000000000000000000000000000000000000000000000000000313131313131313131313131313131313131313131313131313131313131313132000000000000000000000000000000000000000000000000000000000000003300000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(tx.hash().0),
            "b5c09627bb098ce0188e12fb2b80f126a93518b97539d98a9ab2362a7a63e97e"
        );
    }
}
