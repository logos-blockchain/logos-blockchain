use lb_groth16::COMPRESSED_PROOF_SIZE;
use lb_key_management_system_keys::keys::ED25519_SIGNATURE_SIZE;
use nom::{
    IResult,
    error::{Error, ErrorKind},
};

use crate::{
    mantle::{
        MantleTx, Op, SignedMantleTx,
        nom::{NomDecode as _, NomEncode as _},
        ops::codec::{decode_ops_proofs, encode_ops_proofs},
        transactions::{MantleTxGasContext, Ops},
    },
    proofs::channel_multi_sig_proof::codec::calculate_channel_multi_sig_proof_byte_size,
};

pub fn decode_mantle_tx(input: &[u8]) -> IResult<&[u8], MantleTx> {
    // MantleTx = Ops ExecutionGasPrice StorageGasPrice
    let (input, ops) = Ops::decode(input)?;

    Ok((input, MantleTx(ops)))
}

pub fn decode_signed_mantle_tx(input: &[u8]) -> IResult<&[u8], SignedMantleTx> {
    // SignedMantleTx = MantleTx OpsProofs
    let (input, mantle_tx) = decode_mantle_tx(input)?;
    let (input, ops_proofs) = decode_ops_proofs(input, mantle_tx.ops())?;

    let signed_tx = SignedMantleTx::new(mantle_tx, ops_proofs)
        .map_err(|_| nom::Err::Error(Error::new(input, ErrorKind::Verify)))?;

    Ok((input, signed_tx))
}

#[must_use]
pub fn encode_mantle_tx(tx: &MantleTx) -> Vec<u8> {
    tx.ops().encode()
}

#[must_use]
pub fn encode_signed_mantle_tx(tx: &SignedMantleTx) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_mantle_tx(&tx.mantle_tx));
    bytes.extend(encode_ops_proofs(&tx.ops_proofs, tx.mantle_tx.ops()));
    bytes
}

#[must_use]
pub fn predict_signed_mantle_tx_size(tx: &MantleTx, context: &MantleTxGasContext) -> usize {
    let mantle_tx_size = encode_mantle_tx(tx).len();

    let ops_proofs_size = tx
        .ops()
        .iter()
        .map(|op| match op {
            // Ed25519SigProof = Ed25519Signature
            Op::ChannelInscribe(_) => ED25519_SIGNATURE_SIZE,

            // ChannelMultiSigProof — for an existing channel, threshold sigs;
            // for a new channel (just-in-time created here), no sigs required.
            // TODO: underpredicts if there is a non-empty proof for a new
            // channel. Tighten before enabling non-zero storage gas prices.
            Op::ChannelConfig(operation) => {
                let threshold = context
                    .configuration_threshold(&operation.channel)
                    .unwrap_or(0);
                calculate_channel_multi_sig_proof_byte_size(threshold)
            }

            // ZkAndEd25519SigsProof = ZkSignature Ed25519Signature
            Op::SDPDeclare(_) => COMPRESSED_PROOF_SIZE + ED25519_SIGNATURE_SIZE,

            // ZkSigProof = ZkSignature = ProofOfClaimProof = Groth16
            Op::SDPWithdraw(_) | Op::SDPActive(_) | Op::LeaderClaim(_) | Op::Transfer(_) => {
                COMPRESSED_PROOF_SIZE
            }

            // ChannelMultiSigProof
            Op::ChannelWithdraw(operation) => {
                let channel_withdraw_threshold = context.withdraw_threshold(&operation.channel_id).expect(
                    "Operation should have been verified before reaching this point, so the channel must exist in the context."
                );
                calculate_channel_multi_sig_proof_byte_size(channel_withdraw_threshold)
            }

            // None
            Op::ChannelDeposit(_) => 0,
        })
        .sum::<usize>();

    mantle_tx_size + ops_proofs_size
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ark_ff::AdditiveGroup as _;
    use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
    use lb_groth16::{CompressedGroth16Proof, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature, ZkKey, ZkPublicKey};
    use lb_utils::bounded::BoundedError;
    use multiaddr::Multiaddr;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{
            Note, NoteId, OpProof, Transaction as _, Utxo,
            ledger::{BoundedInputs, BoundedOutputs, Inputs, Outputs},
            ops::{
                channel::{
                    ChannelId, MsgId,
                    config::{ChannelConfigOp, Keys},
                    inscribe,
                    inscribe::{Inscription, InscriptionOp},
                    withdraw::ChannelWithdrawOp,
                },
                leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier},
                sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
                transfer::TransferOp,
            },
            transactions::GasPrices,
        },
        proofs::{
            channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
            leader_claim_proof::Groth16LeaderClaimProof,
        },
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
    fn test_decode_signed_mantle_tx_empty() {
        let mantle_tx = MantleTx(Ops::new_unchecked(vec![]));

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
        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::ChannelInscribe(
            InscriptionOp {
                channel_id: ChannelId::from([0xAA; 32]),
                inscription: b"hello".into(),
                parent: MsgId::from([0xBB; 32]),
                signer: signing_key.public_key(),
            },
        )]));

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
        let mantle_tx = MantleTx(Ops::new_unchecked(vec![
            Op::ChannelInscribe(InscriptionOp {
                channel_id: ChannelId::from([0x11; 32]),
                inscription: b"first".into(),
                parent: MsgId::from([0x00; 32]),
                signer: signing_key.public_key(),
            }),
            Op::ChannelConfig(ChannelConfigOp {
                channel: ChannelId::from([0x22; 32]),
                keys: signing_key.public_key().into(),
                posting_timeframe: 1.into(),
                posting_timeout: 2.into(),
                configuration_threshold: 3,
                withdraw_threshold: 4,
            }),
        ]));

        let txhash = mantle_tx.hash();
        let sig = signing_key.sign_payload(&txhash.as_signing_bytes());

        // ChannelConfig creates the channel just-in-time, so no signatures are
        // required for validation — empty proof is well-formed.
        let config_proof = ChannelMultiSigProof::try_new([].into()).unwrap();

        // Encode and decode roundtrip test (no hardcoded test vector since signatures
        // are deterministic)
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(sig),
                OpProof::ChannelMultiSigProof(config_proof),
            ],
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
                    signer: signing_key.public_key(),
                };

                let mantle_tx =
                    MantleTx(Ops::new_unchecked(vec![Op::ChannelInscribe(inscribe_op)]));

                let txhash = mantle_tx.hash();
                let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
                let signed_tx =
                    SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(op_sig)]).unwrap();

                let encoded = encode_signed_mantle_tx(&signed_tx);

                let gas_context =
                    MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
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
        let original_tx = MantleTx(Ops::new_unchecked(vec![]));

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
        // Create a MantleTx with ledger inputs and outputs
        let pk = ZkPublicKey::from(BigUint::from(42u64));
        let note = Note::new(1000, pk);
        let note_id = NoteId(BigUint::from(123u64).into());
        let transfer_op = TransferOp::new(Inputs::new([note_id]), Outputs::new([note]));

        let original_tx = MantleTx(Ops::new_unchecked(vec![Op::Transfer(transfer_op)]));

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
        let mantle_tx = MantleTx(Ops::new_unchecked(vec![]));
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
        let mantle_tx = MantleTx(Ops::new_unchecked(vec![]));

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
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
            inscription: b"hello world".into(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        };

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::ChannelInscribe(inscribe_op)]));

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
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

        let config_op = ChannelConfigOp {
            channel: ChannelId::from([0xFF; 32]),
            keys: [
                signing_key1.public_key(),
                signing_key2.public_key(),
                signing_key3.public_key(),
            ]
            .into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
            withdraw_threshold: 0,
        };

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::ChannelConfig(config_op)]));

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get the actual size.
        // New channel → empty proof (no signatures required for just-in-time create).
        let config_proof = ChannelMultiSigProof::try_new([].into()).unwrap();
        let signed_tx =
            SignedMantleTx::new(mantle_tx, vec![OpProof::ChannelMultiSigProof(config_proof)])
                .unwrap();
        let encoded = encode_signed_mantle_tx(&signed_tx);
        let actual_size = encoded.len();

        assert_eq!(predicted_size, actual_size);
    }

    #[test]
    fn test_predict_signed_mantle_tx_size_with_sdp_declare() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let zk_sk = ZkKey::zero();
        let locator1: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        let locator2: Multiaddr = "/ip6/::1/tcp/9090".parse().unwrap();

        let locked_note_sk = ZkKey::from(BigUint::from(1u64));
        let locked_note = Utxo {
            op_id: [1u8; 32],
            output_index: 12,
            note: Note {
                value: 500,
                pk: locked_note_sk.to_public_key(),
            },
        };
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
            locked_note_id: locked_note.id(),
        };

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::SDPDeclare(sdp_declare_op)]));

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
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

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::SDPWithdraw(sdp_withdraw_op)]));

        let txhash = mantle_tx.hash();

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
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
        let signing_key = Ed25519Key::from_bytes(&[1u8; 32]);
        let blend_proof = ActivityProof {
            epoch: 42.into(),
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

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::SDPActive(sdp_active_op)]));

        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
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
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: b"test".into(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        };

        let config_op = ChannelConfigOp {
            channel: ChannelId::from([0xCC; 32]),
            keys: signing_key.public_key().into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
            withdraw_threshold: 0,
        };

        let blend_proof = ActivityProof {
            epoch: u32::MAX.into(),
            signing_key: signing_key.public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0u8; 160]).into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0u8; 32]).into(),
        };

        let sdp_active_op = SDPActiveOp {
            declaration_id: DeclarationId([0x33; 32]),
            nonce: 55,
            metadata: ActivityMetadata::Blend(Box::new(blend_proof)),
        };

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelConfig(config_op),
            Op::SDPActive(sdp_active_op),
        ]));

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        let txhash = mantle_tx.hash();
        let op_sig = signing_key.sign_payload(&txhash.as_signing_bytes());
        // Create a signed tx and encode it to get the actual size.
        // ChannelConfig creates the channel here, so its proof has no signatures.
        let config_proof = ChannelMultiSigProof::try_new([].into()).unwrap();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(op_sig),
                OpProof::ChannelMultiSigProof(config_proof),
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
        let pk1 = ZkPublicKey::from(BigUint::from(100u64));
        let pk2 = ZkPublicKey::from(BigUint::from(200u64));

        let note1 = Note::new(1000, pk1);
        let note2 = Note::new(2000, pk2);

        let note_id1 = NoteId(BigUint::from(111u64).into());
        let note_id2 = NoteId(BigUint::from(222u64).into());
        let note_id3 = NoteId(BigUint::from(333u64).into());

        let transfer_op = TransferOp::new(
            Inputs::new([note_id1, note_id2, note_id3]),
            Outputs::new([note1, note2]),
        );

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::Transfer(transfer_op)]));

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
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
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);

        let inscribe_op = InscriptionOp {
            channel_id: ChannelId::from([0x11; 32]),
            inscription: b"complex test inscription with more data".into(),
            parent: MsgId::from([0x22; 32]),
            signer: signing_key1.public_key(),
        };

        let config_op = ChannelConfigOp {
            channel: ChannelId::from([0x33; 32]),
            keys: [signing_key1.public_key(), signing_key2.public_key()].into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
            withdraw_threshold: 0,
        };

        let locked_note_sk = ZkKey::from(BigUint::from(1u64));
        let transfer_op = TransferOp {
            inputs: Inputs::new([NoteId(BigUint::from(777u64).into())]),
            outputs: Outputs::new([Note::new(5000, locked_note_sk.to_public_key())]),
        };

        let locator: Multiaddr = "/dns4/example.com/tcp/443".parse().unwrap();
        let zk_sk = ZkKey::zero();
        let sdp_declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: Locator::new_unchecked(locator).into(),
            provider_id: ProviderId(signing_key1.public_key()),
            zk_id: zk_sk.to_public_key(),
            locked_note_id: transfer_op
                .outputs
                .utxo_by_index(0, &transfer_op)
                .unwrap()
                .id(),
        };

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelConfig(config_op),
            Op::SDPDeclare(sdp_declare_op),
            Op::Transfer(transfer_op),
        ]));

        // Predict size
        let gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &gas_context);

        // Create a signed tx and encode it to get the actual size.
        // ChannelConfig creates the channel here, so its proof has no signatures.
        let txhash = mantle_tx.hash();
        let op_ed25519_sig = signing_key1.sign_payload(&txhash.as_signing_bytes());
        let config_proof = ChannelMultiSigProof::try_new([].into()).unwrap();
        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(op_ed25519_sig),
                OpProof::ChannelMultiSigProof(config_proof),
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
        let leader_claim_op = LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: VoucherNullifier::default(),
            pk: ZkPublicKey::from(BigUint::from(0u64)),
        };

        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::LeaderClaim(leader_claim_op)]));

        let empty_gas_context =
            MantleTxGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(0, 0));
        let predicted_size = predict_signed_mantle_tx_size(&mantle_tx, &empty_gas_context);

        let poc_proof =
            Groth16LeaderClaimProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128]));

        // Construct directly to skip proof verification (dummy proof won't verify)
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![OpProof::PoC(poc_proof)],
        };

        let encoded = encode_signed_mantle_tx(&signed_tx);
        assert_eq!(predicted_size, encoded.len());
    }

    #[test]
    fn test_encode_decode_leader_claim_op() {
        let leader_claim_op = LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: VoucherNullifier::default(),
            pk: ZkPublicKey::from(BigUint::from(0u64)),
        };
        let op = Op::LeaderClaim(leader_claim_op);

        let encoded = op.encode();
        let (remaining, decoded_op) = Op::decode(&encoded).unwrap();
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
        let mantle_tx = MantleTx(Ops::new_unchecked(vec![Op::ChannelWithdraw(
            ChannelWithdrawOp {
                channel_id: ChannelId::from([0xAB; 32]),
                outputs: Outputs::new([note1, note2]),
                withdraw_nonce: 0,
            },
        )]));
        let tx_hash = mantle_tx.hash();
        let proof = ChannelMultiSigProof::try_new(
            [IndexedSignature::new(
                0,
                signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )]
            .into(),
        )
        .unwrap();
        let signed_tx =
            SignedMantleTx::new(mantle_tx, vec![OpProof::ChannelMultiSigProof(proof)]).unwrap();

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
        let oversized_inscription = vec![0xAB; inscribe::MAX_BYTES + 1];

        let result = Inscription::try_from(oversized_inscription);
        assert_eq!(
            result,
            Err(BoundedError::TooManyItems {
                count: inscribe::MAX_BYTES + 1,
                max: inscribe::MAX_BYTES
            })
        );
    }

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
            Op::ChannelConfig(ChannelConfigOp {
                channel: ChannelId::from([0x22; 32]),
                keys: Ed25519Key::from_bytes(&[1; 32]).public_key().into(),
                posting_timeframe: 0.into(),
                posting_timeout: 0.into(),
                configuration_threshold: 0,
                withdraw_threshold: 0,
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
        valid_input.extend_from_slice(&(inscribe::MAX_BYTES as u32).to_le_bytes());

        // Inscription data (MAX_INSCRIPTION_SIZE bytes)
        valid_input.extend_from_slice(&vec![0x01; inscribe::MAX_BYTES]);

        // Parent MsgId (32 bytes)
        valid_input.extend_from_slice(&[0x43; 32]);

        // Signer Ed25519PublicKey (32 bytes)
        let sk = Ed25519Key::from_bytes(&[0x44; 32]);
        let pk = sk.public_key();
        valid_input.extend_from_slice(&pk.to_bytes());

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
            // Using `new_unchecked` to bypass the constructor check since we're testing
            // `decode` directly.
            keys: Keys::new_unchecked([].into()),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 0,
            withdraw_threshold: 0,
        }
        .encode();

        assert_eq!(
            ChannelConfigOp::decode(&encoded_config_op).unwrap_err(),
            nom::Err::Error(Error {
                input: &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..],
                code: ErrorKind::LengthValue,
            }),
        );
    }

    #[test]
    fn test_decode_accept_max_key_count() {
        // Test that key_count = MAX_KEY_COUNT is accepted
        let mut valid_input = Vec::new();

        // ChannelId (32 bytes)
        valid_input.extend_from_slice(&[0x42; 32]);

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
        if let Err(nom::Err::Error(ref e)) = result {
            assert_eq!(
                e.code,
                ErrorKind::TooLarge,
                "Should reject at `MAX_LOCATOR_BYTE_SIZE + 1`"
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

        let oversized_len = MAX_LOCATOR_BYTE_SIZE as u16;
        malicious_input.extend_from_slice(&oversized_len.to_le_bytes());

        // Add the oversized data
        malicious_input.extend_from_slice(&vec![0x01; MAX_LOCATOR_BYTE_SIZE]);

        // ... rest of SDPDeclare fields ...

        let result = SDPDeclareOp::decode(&malicious_input);
        if let Err(nom::Err::Error(ref e)) = result {
            assert_ne!(
                e.code,
                ErrorKind::LengthValue,
                "Should not reject at `MAX_LOCATOR_BYTE_SIZE`"
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
            locked_note_id: NoteId(BigUint::from(111u64).into()),
        };

        let encoded = op.encode();
        let result = SDPDeclareOp::decode(&encoded);

        match result {
            Err(nom::Err::Error(e)) => assert_eq!(e.code, ErrorKind::MapRes),
            _ => panic!("Expected Verify error for invalid locator"),
        }
    }

    #[test]
    fn test_encode_decode_max_inputs() {
        let note_id = NoteId(BigUint::from(111u64).into());
        let inputs = [note_id; u8::MAX as usize];
        let inputs = BoundedInputs::from(inputs);

        // Encode should succeed
        let encoded = inputs.encode();
        assert!(
            !encoded.is_empty(),
            "Encoding max input count should produce some output"
        );

        // Decode should succeed and produce the same number of inputs
        let result = BoundedInputs::decode(&encoded);
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
        let outputs = BoundedOutputs::from(outputs);

        // Encode should succeed
        let encoded = outputs.encode();
        assert!(
            !encoded.is_empty(),
            "Encoding max output count should produce some output"
        );

        // Decode should succeed and produce the same number of outputs
        let result = BoundedOutputs::decode(&encoded);
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

        let result = BoundedInputs::decode(&valid_input);
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

        let result = BoundedOutputs::decode(&valid_output);
        assert!(result.is_ok(), "Should accept max output count");
        let (_, outputs) = result.unwrap();
        assert_eq!(outputs.len(), u8::MAX as usize);
    }
}
