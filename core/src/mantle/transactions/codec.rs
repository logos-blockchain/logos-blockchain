use lb_binary_codec::canonical::BinaryEncode as _;
use lb_groth16::COMPRESSED_PROOF_SIZE;
use lb_key_management_system_keys::keys::ED25519_SIGNATURE_SIZE;

use crate::{
    mantle::{
        OpRef,
        gas::ThresholdSource as _,
        transactions::{
            thresholds::TxGasState,
            tx_list::{OpRefs, ops::OpsGasContext},
        },
    },
    proofs::channel_multi_sig_proof::codec::calculate_channel_multi_sig_proof_byte_size,
};

/// Predicts the minimum encoded size of the transaction once signed.
///
/// Proof sizes are fixed per op type or dictated by the channel thresholds
/// the ledger enforces, a channel-creating `ChannelConfig` being verified
/// against a threshold of 0.
#[must_use]
pub fn minimum_signed_transaction_size(op_refs: &OpRefs<'_>, context: &OpsGasContext) -> usize {
    let encoded_ops_size = op_refs.encoded_length();

    let (_, ops_proofs_size) = op_refs.iter().fold(
        (TxGasState::new(context), 0),
        |(mut thresholds, total), op| {
            let proof_size = match op {
                // ChannelInscribeOpProof = Ed25519Signature *StepProof
                OpRef::ChannelInscribe(operation) => {
                    ED25519_SIGNATURE_SIZE
                        + operation
                            .steps
                            .iter()
                            .map(|step| step.proof_shape().encoded_length())
                            .sum::<usize>()
                }

                // ChannelMultiSigProof
                //
                // The ledger enforces exactly `configuration_threshold` proofs,
                // which is 0 for a channel that does not exist yet.
                OpRef::ChannelConfig(operation) => calculate_channel_multi_sig_proof_byte_size(
                    thresholds.configuration_threshold(&operation.channel),
                ),

                // ZkTransferAndEd25519SigProof = ZkTransfer Ed25519Signature
                OpRef::SDPDeclare(_) | OpRef::SDPWithdraw(_) => {
                    COMPRESSED_PROOF_SIZE + ED25519_SIGNATURE_SIZE
                }

                // Ed25519SigProof = Ed25519Signature
                OpRef::SDPActive(_) => ED25519_SIGNATURE_SIZE,

                // ZkTransferProof = ZkTransfer = Groth16Proof
                OpRef::Transfer(_) | OpRef::ChannelDeposit(_) | OpRef::ChannelWithdraw(_) => {
                    COMPRESSED_PROOF_SIZE
                }

                OpRef::ClaimPowReward(_) | OpRef::ProgramRegister(_) => 0, // no proof
            };
            thresholds.apply(*op);
            (thresholds, total + proof_size)
        },
    );

    encoded_ops_size + ops_proofs_size
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ark_ff::{AdditiveGroup as _, Field as _};
    use lb_binary_codec::canonical::{BinaryDecodeExt as _, DecodeError};
    use lb_blend_proofs::{
        quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
        selection::VerifiedProofOfSelection,
    };
    use lb_groth16::{CompressedGroth16Proof, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature, ZkKey};
    use lb_utils::bounded::BoundedError;
    use multiaddr::Multiaddr;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{
            Op, OpProof,
            ledger::verification_mode::StandardMode,
            notes::{InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments},
            ops::{
                ChannelInscribeProof, StepProof, ZkAndEd25519Proof, ZkTransferProof,
                channel::{
                    ChannelId, MsgId, VerifiedChannelKeys,
                    config::ChannelConfigOp,
                    inscribe::{self, ChannelStep, ChannelSteps, Inscription, InscriptionOp, Step},
                    withdraw::ChannelWithdrawOp,
                },
                op_proof::samples::SampleProof as _,
                sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
                transfer::TransferOp,
            },
            traits::{Hashable as _, MantleTx as _},
            transactions::{GasPrices, OpProofs, SignedOps, states::Unverified, tx_list::Ops},
        },
        proofs::channel_multi_sig_proof::ChannelMultiSigProof,
        sdp::{
            ActivityMetadata, DeclarationId, Locator, MAX_LOCATOR_BYTE_SIZE, ProviderId,
            ServiceType, blend::ActivityProof,
        },
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
    fn test_decode_signed_mantle_tx_with_inscribe() {
        let signing_key = Ed25519Key::from_bytes(&[4u8; 32]);
        let tx = Ops::from([Op::ChannelInscribe(InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"hello".into(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key().into_unverified(),
            steps: ChannelSteps::default(),
        })]);

        let tx_hash = tx.hash();
        let inscribe_sig = OpProof::ChannelInscribe(ChannelInscribeProof::without_steps(
            signing_key.sign_payload(tx_hash.as_signing_bytes()),
        ));
        let op_proofs = OpProofs::from([inscribe_sig]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(tx, op_proofs).unwrap();

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
            + "0000"                                                             // StepCount
            + "27e257e71726bad757bf67bf377009c37f7403117b6cc841216bfc5679a8e1a3" // Signature (64Byte)
            + "55a193639cc35b06b4d0992279c95292237e403d68ebbb543773b3311b4aca0c";

        // ENCODING
        let encoded = hex::encode(signed_tx.encode());
        if encoded != test_vector {
            dbg_test_vector(&encoded, &test_vector);
            assert_eq!(encoded, test_vector);
        }

        // DECODING
        let test_vector_bytes = hex::decode(test_vector).unwrap();
        let (remaining, decoded_tx) = SignedOps::decode(&test_vector_bytes).unwrap();
        assert_eq!(remaining, b"");
        assert_eq!(decoded_tx, signed_tx);
    }
    #[test]
    fn test_decode_signed_mantle_tx_with_multiple_ops() {
        let signing_key = Ed25519Key::from_bytes(&[4u8; 32]);
        let tx = Ops::new_unchecked(vec![
            Op::ChannelInscribe(InscriptionOp {
                channel_id: ChannelId::from([0x11; 32]),
                inscription: b"first".into(),
                parent: MsgId::from([0x00; 32]),
                signer: signing_key.public_key().into_unverified(),
                steps: ChannelSteps::default(),
            }),
            Op::ChannelConfig(ChannelConfigOp {
                channel: ChannelId::from([0x22; 32]),
                parent: MsgId::from([0x33; 32]),
                keys: signing_key.public_key().into(),
                posting_timeframe: 1.into(),
                posting_timeout: 2.into(),
                configuration_threshold: 3,
            }),
        ]);

        let tx_hash = tx.hash();
        let sig = signing_key.sign_payload(tx_hash.as_signing_bytes());

        // ChannelConfig creates the channel just-in-time, so no signatures are
        // required for validation — empty proof is well-formed.
        let config_proof = ChannelMultiSigProof::empty();

        // Encode and decode roundtrip test (no hardcoded test vector since signatures
        // are deterministic)
        let op_proofs = OpProofs::from([
            OpProof::ChannelInscribe(ChannelInscribeProof::without_steps(sig)),
            OpProof::ChannelMultiSigProof(config_proof),
        ]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(tx, op_proofs).unwrap();

        let encoded = signed_tx.encode();
        let (remaining, decoded_tx) = SignedOps::decode(&encoded).unwrap();
        assert_eq!(remaining, b"");
        assert_eq!(decoded_tx, signed_tx);
    }

    #[tokio::test]
    async fn test_large_payload_encoding_decoding() {
        // Test payload sizes from 512kB up to 2MiB in 512kB increments
        const MAX_SIZE: usize = inscribe::MAX_BYTES;
        const CHUNK_SIZE: usize = MAX_SIZE / 10;

        let signing_key = Ed25519Key::from_bytes(&[1; 32]);

        let mut tasks = Vec::new();

        for payload_size in (CHUNK_SIZE..=MAX_SIZE).step_by(CHUNK_SIZE) {
            let signing_key = signing_key.clone();

            let task = tokio::task::spawn(async move {
                let large_inscription = Inscription::new_unchecked(vec![0xAB; payload_size]);

                let inscribe_op = InscriptionOp {
                    channel_id: ChannelId::from([0xAA; 32]),
                    inscription: large_inscription,
                    parent: MsgId::from([0xBB; 32]),
                    signer: signing_key.public_key().into_unverified(),
                    steps: ChannelSteps::default(),
                };

                let tx = Ops::new_unchecked(vec![Op::ChannelInscribe(inscribe_op)]);

                let tx_hash = tx.hash();
                let op_sig = signing_key.sign_payload(tx_hash.as_signing_bytes());
                let op_proofs = OpProofs::from([OpProof::ChannelInscribe(
                    ChannelInscribeProof::without_steps(op_sig),
                )]);
                let signed_ops = SignedOps::<_, StandardMode>::from_parts(tx, op_proofs).unwrap();

                let encoded = signed_ops.encode();

                let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
                let predicted_size =
                    minimum_signed_transaction_size(&signed_ops.op_refs(), &gas_context);
                assert_eq!(
                    predicted_size,
                    encoded.len(),
                    "Size mismatch at payload size {payload_size}",
                );

                let (remaining, decoded_tx) = SignedOps::decode(&encoded)
                    .unwrap_or_else(|_| panic!("Failed to decode at payload size {payload_size}"));

                assert!(
                    remaining.is_empty(),
                    "Unexpected remaining bytes at payload size {payload_size}",
                );
                assert_eq!(
                    decoded_tx, signed_ops,
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
    fn test_encode_decode_roundtrip_with_transfer() {
        // Create a MantleTx with ledger inputs and outputs
        let transfer_op = TransferOp::new(
            [NoteNullifier(Fr::from(123u64))].into(),
            [NoteCommitment(Fr::from(1000u64))].into(),
            Fr::from(5u64),
            6,
        );

        let original_tx = Ops::new_unchecked(vec![Op::Transfer(transfer_op)]);

        // Encode
        let encoded = original_tx.encode();

        // Decode
        let (remaining, decoded_tx) = Ops::decode(&encoded).unwrap();

        // Verify
        assert_eq!(remaining, b"");
        assert_eq!(original_tx, decoded_tx);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_empty_tx() {
        // Create an empty MantleTx
        let mantle_tx = Ops::new_unchecked(vec![]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        // Create a signed tx and encode it to get actual size
        let op_proofs = OpProofs::empty();
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_with_inscribe() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"hello world".into(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key().into_unverified(),
            steps: ChannelSteps::default(),
        };

        let mantle_tx = Ops::new_unchecked(vec![Op::ChannelInscribe(inscribe_op)]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        // Create a signed tx and encode it to get actual size
        let tx_hash = mantle_tx.hash();
        let op_sig = signing_key.sign_payload(tx_hash.as_signing_bytes());
        let op_proofs = OpProofs::from([OpProof::ChannelInscribe(
            ChannelInscribeProof::without_steps(op_sig),
        )]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn minimum_size_counts_one_zk_transfer_per_step() {
        let step = ChannelStep {
            inputs: [NoteNullifier(Fr::ONE)].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: Fr::ZERO,
            buffered: 0,
        };
        let ops = Ops::from([Op::ChannelInscribe(InscriptionOp {
            steps: [Step::User(step.clone()), Step::User(step)].into(),
            ..InscriptionOp::sample()
        })]);
        let proof = ChannelInscribeProof::new(
            Ed25519Signature::zero(),
            vec![
                StepProof::User(ZkTransferProof::sample()),
                StepProof::User(ZkTransferProof::sample()),
            ],
        );
        let signed = SignedOps::<Unverified, StandardMode>::from_parts(
            ops.clone(),
            OpProofs::from([OpProof::ChannelInscribe(proof)]),
        )
        .unwrap();
        assert_eq!(
            minimum_signed_transaction_size(&ops.by_ref(), &OpsGasContext::default()),
            signed.encode().len()
        );
    }

    #[test]
    fn minimum_size_counts_the_proofs_and_openings_of_a_program_step() {
        let operation = InscriptionOp::sample_with_step();
        let ops = Ops::from([Op::ChannelInscribe(operation.clone())]);
        let proof = Op::ChannelInscribe(operation).sample_proof();
        assert_eq!(proof.encode().len(), 64 + 128 + (128 + 128 + 128 + 104));
        let signed =
            SignedOps::<Unverified, StandardMode>::from_parts(ops.clone(), OpProofs::from([proof]))
                .unwrap();
        assert_eq!(
            minimum_signed_transaction_size(&ops.by_ref(), &OpsGasContext::default()),
            signed.encode().len()
        );
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_with_set_keys() {
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);
        let signing_key3 = Ed25519Key::from_bytes(&[3; 32]);

        let config_op = ChannelConfigOp {
            channel: ChannelId::from([0xFF; 32]),
            parent: MsgId::from([0xAA; 32]),
            keys: [
                signing_key1.public_key(),
                signing_key2.public_key(),
                signing_key3.public_key(),
            ]
            .into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
        };

        let mantle_tx = Ops::new_unchecked(vec![Op::ChannelConfig(config_op)]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        // Create a signed tx and encode it to get the actual size.
        // New channel → empty proof (no signatures required for just-in-time create).
        let config_proof = ChannelMultiSigProof::empty();
        let op_proofs = OpProofs::from([OpProof::ChannelMultiSigProof(config_proof)]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_with_sdp_declare() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let zk_sk = ZkKey::zero();
        let locator1: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        let locator2: Multiaddr = "/ip6/::1/tcp/9090".parse().unwrap();

        let sdp_declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec![
                Locator::new_unchecked(locator1),
                Locator::new_unchecked(locator2),
            ]
            .try_into()
            .unwrap(),
            provider_id: ProviderId(signing_key.public_key()),
            zk_id: zk_sk.to_public_key(),
            inputs: [NoteNullifier(Fr::from(500u64))].into(),
            commitment_merkle_root: Fr::ZERO,
            amount: 500,
        };

        let mantle_tx = Ops::new_unchecked(vec![Op::SDPDeclare(sdp_declare_op)]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        // Create a signed tx and encode it to get actual size
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::sample(),
            ed25519_sig: Ed25519Signature::from_bytes(&[0u8; 64]),
        };
        let op_proofs = OpProofs::from([OpProof::ZkAndEd25519Sigs(proof)]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_with_sdp_withdraw() {
        let sdp_withdraw_op = SDPWithdrawOp {
            declaration_id: DeclarationId([0x11; 32]),
            nonce: 42,
            service_note_nullifier: NoteNullifier(Fr::from(123u64)),
            outputs: [NoteCommitment(Fr::from(124u64))].into(),
            excess_value: 1,
        };

        let mantle_tx = Ops::new_unchecked(vec![Op::SDPWithdraw(sdp_withdraw_op)]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        // Create a signed tx and encode it to get actual size
        let op_proofs = OpProofs::from([OpProof::ZkAndEd25519Sigs(ZkAndEd25519Proof::sample())]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_with_sdp_active() {
        let signing_key = Ed25519Key::from_bytes(&[1u8; 32]);
        let blend_proof = ActivityProof {
            epoch: 42.into(),
            signing_key: signing_key.public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0u8; PROOF_OF_QUOTA_SIZE])
                .into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0u8; 32]).into(),
        };

        let metadata = ActivityMetadata::Blend(Box::new(blend_proof));

        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x22; 32]),
            nonce: 99,
            metadata,
        };

        let mantle_tx = Ops::new_unchecked(vec![Op::SDPActive(sdp_active_op)]);

        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
            &[0u8; 64],
        ))]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();

        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_with_multiple_ops() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"test".into(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key().into_unverified(),
            steps: ChannelSteps::default(),
        };

        let config_op = ChannelConfigOp {
            channel: ChannelId::from([0xCC; 32]),
            parent: MsgId::from([0xDD; 32]),
            keys: signing_key.public_key().into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
        };

        let blend_proof = ActivityProof {
            epoch: u32::MAX.into(),
            signing_key: signing_key.public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0u8; PROOF_OF_QUOTA_SIZE])
                .into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0u8; 32]).into(),
        };

        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x33; 32]),
            nonce: 55,
            metadata: ActivityMetadata::Blend(Box::new(blend_proof)),
        };

        let mantle_tx = Ops::new_unchecked(vec![
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelConfig(config_op),
            Op::SDPActive(sdp_active_op),
        ]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        let tx_hash = mantle_tx.hash();
        let op_sig = signing_key.sign_payload(tx_hash.as_signing_bytes());
        // Create a signed tx and encode it to get the actual size.
        // ChannelConfig creates the channel here, so its proof has no signatures.
        let config_proof = ChannelMultiSigProof::empty();
        let op_proofs = OpProofs::from([
            OpProof::ChannelInscribe(ChannelInscribeProof::without_steps(op_sig)),
            OpProof::ChannelMultiSigProof(config_proof),
            OpProof::Ed25519Sig(op_sig),
        ]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_with_ledger_inputs_outputs() {
        let transfer_op = TransferOp::new(
            [111u64, 222, 333]
                .map(|n| NoteNullifier(Fr::from(n)))
                .into(),
            [1000u64, 2000].map(|n| NoteCommitment(Fr::from(n))).into(),
            Fr::ZERO,
            0,
        );

        let mantle_tx = Ops::new_unchecked(vec![Op::Transfer(transfer_op)]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        // Create a signed tx and encode it to get actual size
        let op_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::sample())]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_minimum_signed_mantle_tx_size_complex_scenario() {
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0x11; 32]),
            inscription: b"complex test inscription with more data".into(),
            parent: MsgId::from([0x22; 32]),
            signer: signing_key1.public_key().into_unverified(),
            steps: ChannelSteps::default(),
        };

        let config_op = ChannelConfigOp {
            channel: ChannelId::from([0x33; 32]),
            parent: MsgId::from([0x44; 32]),
            keys: [signing_key1.public_key(), signing_key2.public_key()].into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
        };

        let transfer_op = TransferOp::new(
            [NoteNullifier(Fr::from(777u64))].into(),
            [NoteCommitment(Fr::from(5000u64))].into(),
            Fr::ZERO,
            0,
        );

        let locator: Multiaddr = "/dns4/example.com/tcp/443".parse().unwrap();
        let zk_sk = ZkKey::zero();
        let sdp_declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: Locator::new_unchecked(locator).into(),
            provider_id: ProviderId(signing_key1.public_key()),
            zk_id: zk_sk.to_public_key(),
            inputs: [NoteNullifier(Fr::from(500u64))].into(),
            commitment_merkle_root: Fr::ZERO,
            amount: 500,
        };

        let mantle_tx = Ops::new_unchecked(vec![
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelConfig(config_op),
            Op::SDPDeclare(sdp_declare_op),
            Op::Transfer(transfer_op),
        ]);

        // Predict size
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = minimum_signed_transaction_size(&mantle_tx.op_refs(), &gas_context);

        // Create a signed tx and encode it to get the actual size.
        // ChannelConfig creates the channel here, so its proof has no signatures.
        let tx_hash = mantle_tx.hash();
        let op_ed25519_sig = signing_key1.sign_payload(tx_hash.as_signing_bytes());
        let config_proof = ChannelMultiSigProof::empty();
        let zk_and_ed25519_proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::sample(),
            ed25519_sig: op_ed25519_sig,
        };
        let op_proofs = OpProofs::from([
            OpProof::ChannelInscribe(ChannelInscribeProof::without_steps(op_ed25519_sig)),
            OpProof::ChannelMultiSigProof(config_proof),
            OpProof::ZkAndEd25519Sigs(zk_and_ed25519_proof),
            OpProof::ZkTransfer(ZkTransferProof::sample()),
        ]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();
        let encoded = signed_tx.encode();
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_encode_decode_channel_withdraw_tx() {
        let mantle_tx = Ops::new_unchecked(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id: ChannelId::from([0xAB; 32]),
            inputs: [NoteNullifier(Fr::ZERO)].into(),
            commitment_merkle_root: Fr::ZERO,
            outputs: OutputCommitments::default(),
        })]);
        let op_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(
            CompressedGroth16Proof::from_bytes(&[0u8; 128]),
        ))]);
        let signed_tx = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();

        let encoded = signed_tx.encode();
        let (remaining, decoded_tx) = SignedOps::<_, StandardMode>::decode(&encoded).unwrap();

        assert_eq!(remaining, b"");
        assert_eq!(decoded_tx, signed_tx);
    }

    // ==============================================================================
    // Security Tests - Memory Over-Allocation Protection
    // ==============================================================================

    #[test]
    fn test_decode_reject_oversized_inscription() {
        // Create a malicious input with inscription_len = MAX_INSCRIPTION_SIZE + 1
        let mut malicious_input = Vec::new();

        // ChannelId (32 bytes)
        malicious_input.extend_from_slice(&[0x42; 32]);

        // Inscription length (u32) - exceeds MAX_INSCRIPTION_SIZE
        let oversized_len = inscribe::MAX_BYTES + 1;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // We don't need to include the actual inscription data because
        // the decoder should reject it before trying to read that much

        // Try to decode - should fail with TooLarge error
        let result = InscriptionOp::decode(&malicious_input);
        assert!(result.is_err(), "Should reject oversized inscription");

        // Verify it fails with the right error kind
        assert!(
            matches!(result, Err(DecodeError::LengthOutOfBounds { .. })),
            "Expected LengthOutOfBounds error",
        );
    }

    #[test]
    fn test_encode_reject_excessive_op_count() {
        let ops = vec![
            Op::ChannelConfig(ChannelConfigOp {
                channel: ChannelId::from([0x22; 32]),
                parent: MsgId::from([0x33; 32]),
                keys: Ed25519Key::from_bytes(&[1; 32]).public_key().into(),
                posting_timeframe: 0.into(),
                posting_timeout: 0.into(),
                configuration_threshold: 0,
            });
            u8::MAX as usize + 1
        ];

        let result = Ops::try_from(ops);
        assert_eq!(
            result,
            Err(BoundedError::TooManyItems {
                count: u8::MAX as usize + 1,
                max: u8::MAX as usize
            })
        );
    }

    #[test]
    fn test_decode_accept_max_op_count() {
        // Test that op_count = MAX_OP_COUNT is accepted
        // (though it will fail later due to missing op data, which is fine for this
        // test)
        let valid_input = vec![u8::MAX];

        // Should not fail with TooLarge error (will fail with incomplete data)
        let result = Ops::decode(&valid_input);
        if let Err(err) = result {
            assert!(
                !matches!(err, DecodeError::LengthOutOfBounds { .. }),
                "Should not reject at u8::MAX",
            );
        }
    }

    #[test]
    fn test_decode_accept_max_inscription_size() {
        // Test that we can decode an inscription at exactly MAX_INSCRIPTION_SIZE
        let mut valid_input = Vec::new();

        // ChannelId (32 bytes)
        valid_input.extend_from_slice(&[0x42; 32]);

        // Inscription length (u32) - exactly MAX_INSCRIPTION_SIZE
        valid_input.extend_from_slice(&(inscribe::MAX_BYTES as u32).to_le_bytes());

        // Inscription data (MAX_INSCRIPTION_SIZE bytes)
        valid_input.extend_from_slice(&vec![0x01; inscribe::MAX_BYTES]);

        // Parent MsgId (32 bytes)
        valid_input.extend_from_slice(&[0x43; 32]);

        // Signer Ed25519PublicKey (32 bytes)
        let sk = Ed25519Key::from_bytes(&[0x44; 32]);
        let pk = sk.public_key();
        valid_input.extend_from_slice(&pk.to_bytes());

        // Step count (u16), no step
        valid_input.extend_from_slice(&0u16.to_le_bytes());

        // Should succeed (though signature validation might fail later)
        let result = InscriptionOp::decode(&valid_input);
        assert!(
            result.is_ok(),
            "Should accept inscription at MAX_INSCRIPTION_SIZE: {result:?}",
        );

        let (_, inscription_op) = result.unwrap();
        assert_eq!(inscription_op.inscription.len(), inscribe::MAX_BYTES);
    }

    #[test]
    fn test_decode_reject_zero_key_count() {
        let encoded_config_op = ChannelConfigOp {
            channel: ChannelId::from([0x22; 32]),
            parent: MsgId::from([0x33; 32]),
            // Using `new_unchecked` to bypass the constructor check since we're testing
            // `decode` directly.
            keys: VerifiedChannelKeys::new_unchecked([].into()),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
        }
        .encode();

        let err = ChannelConfigOp::decode(&encoded_config_op).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 0, .. }));
    }

    #[test]
    fn test_decode_accept_max_key_count() {
        // Test that key_count = MAX_KEY_COUNT is accepted
        let mut valid_input = Vec::new();

        // ChannelId (32 bytes)
        valid_input.extend_from_slice(&[0x42; 32]);

        // Parent MsgId (32 bytes)
        valid_input.extend_from_slice(&[0x43; 32]);

        // KeyCount = MAX_KEY_COUNT
        valid_input.extend_from_slice(&u16::MAX.encode());

        // Add MAX_KEY_COUNT Ed25519 public keys (each 32 bytes)
        for i in 0..u16::MAX {
            let key_input = {
                let mut input = i.to_le_bytes().to_vec();
                input.resize(32, 0);
                input
            };
            let sk = Ed25519Key::from_bytes(&key_input.try_into().unwrap());
            let pk = sk.public_key();
            valid_input.extend_from_slice(&pk.to_bytes());
        }

        // Posting Timeframe (32 bytes)
        valid_input.extend_from_slice(&[0; 32]);

        // Posting Timeout (32 bytes)
        valid_input.extend_from_slice(&[0; 32]);

        // Configuration Threshold (16 bytes)
        valid_input.extend_from_slice(&[0; 16]);

        // Withdraw Threshold (16 bytes)
        valid_input.extend_from_slice(&[0; 16]);

        let result = ChannelConfigOp::decode(&valid_input);
        assert!(result.is_ok(), "Should accept max key count: {result:?}");

        let (_, set_keys_op) = result.unwrap();
        assert_eq!(set_keys_op.keys.len(), u16::MAX as usize);
    }

    #[test]
    fn test_decode_reject_oversized_locator() {
        // Create a malicious input with oversized locator
        let mut malicious_input = Vec::new();

        // ServiceType (1 byte)
        malicious_input.push(0x00);

        // LocatorCount (1 byte) - just 1 locator
        malicious_input.push(1);

        let oversized_len = (MAX_LOCATOR_BYTE_SIZE + 1) as u16;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // Add the oversized data
        malicious_input.extend_from_slice(&vec![0x01; MAX_LOCATOR_BYTE_SIZE + 1]);

        // ... rest of SDPDeclare fields ...

        let result = SDPDeclareOp::decode(&malicious_input);
        assert!(
            matches!(result, Err(DecodeError::LengthOutOfBounds { .. })),
            "Should reject at `MAX_LOCATOR_BYTE_SIZE + 1`",
        );
    }

    #[test]
    fn test_decode_accept_max_locator_size() {
        // Create a malicious input with oversized locator
        let mut malicious_input = Vec::new();

        // ServiceType (1 byte)
        malicious_input.push(0x00);

        // LocatorCount (1 byte) - just 1 locator
        malicious_input.push(1);

        let oversized_len = MAX_LOCATOR_BYTE_SIZE as u16;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // Add the oversized data
        malicious_input.extend_from_slice(&vec![0x01; MAX_LOCATOR_BYTE_SIZE]);

        // ... rest of SDPDeclare fields ...

        let result = SDPDeclareOp::decode(&malicious_input);
        if let Err(ref err) = result {
            assert!(
                !matches!(err, DecodeError::LengthOutOfBounds { .. }),
                "Should not reject at `MAX_LOCATOR_BYTE_SIZE`",
            );
        }
        assert!(result.is_err(), "Should reject invalid declaration");
    }

    #[test]
    fn decode_reject_invalid_locator() {
        let invalid_locator: Multiaddr = "/ip4/0.0.0.0/udp/3000/quic-v1"
            .parse()
            .expect("locator should parse as multiaddr");
        let op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: Locator::new_unchecked(invalid_locator).into(),
            provider_id: ProviderId(Ed25519Key::from_bytes(&[1; 32]).public_key()),
            zk_id: ZkKey::zero().to_public_key(),
            inputs: [NoteNullifier(Fr::from(500u64))].into(),
            commitment_merkle_root: Fr::ZERO,
            amount: 500,
        };

        let encoded = op.encode();
        let result = SDPDeclareOp::decode(&encoded);

        assert!(
            matches!(result, Err(DecodeError::InvalidValue { .. })),
            "Expected an invalid-value error for invalid locator",
        );
    }

    #[test]
    fn test_encode_decode_max_inputs() {
        let inputs = (0..u8::MAX).map(|index| NoteNullifier(BigUint::from(index).into()));
        let inputs = InputNullifiers::try_from_iter(inputs).unwrap();

        // Encode should succeed
        let encoded = inputs.encode();
        assert!(
            !encoded.is_empty(),
            "Encoding max input count should produce some output"
        );

        // Decode should succeed and produce the same number of inputs
        let result = InputNullifiers::decode(&encoded);
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
        let note = NoteCommitment(BigUint::from(42u64).into());
        let outputs = [note; u8::MAX as usize];
        let outputs = OutputCommitments::from(outputs);

        // Encode should succeed
        let encoded = outputs.encode();
        assert!(
            !encoded.is_empty(),
            "Encoding max output count should produce some output"
        );

        // Decode should succeed and produce the same number of outputs
        let result = OutputCommitments::decode(&encoded);
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

        // Add MAX_INPUT_COUNT distinct field elements (each 32 bytes, little
        // endian)
        for index in 0..u8::MAX {
            let mut note_nullifier = [0x01; 32];
            note_nullifier[0] = index;
            valid_input.extend_from_slice(&note_nullifier);
        }

        let result = InputNullifiers::decode(&valid_input);
        assert!(result.is_ok(), "Should accept max input count");
        let (_, inputs) = result.unwrap();
        assert_eq!(inputs.len(), u8::MAX as usize);

        // Test that output_count = MAX_OUTPUT_COUNT works
        let mut valid_output = u8::MAX.to_le_bytes().to_vec();

        // Add MAX_OUTPUT_COUNT note commitments (each 32 bytes)
        for _ in 0..u8::MAX {
            valid_output.extend_from_slice(&[0x02; 32]);
        }

        let result = OutputCommitments::decode(&valid_output);
        assert!(result.is_ok(), "Should accept max output count");
        let (_, outputs) = result.unwrap();
        assert_eq!(outputs.len(), u8::MAX as usize);
    }
}
