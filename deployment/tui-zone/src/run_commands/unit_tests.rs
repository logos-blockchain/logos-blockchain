#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use lb_core::mantle::{
        MantleTx, Note, Op, SignedMantleTx, Transaction as _, Utxo, Value,
        ledger::{Inputs, Outputs},
        nom::NomEncode as _,
        ops::channel::{
            ChannelId, MsgId,
            inscribe::{Inscription, InscriptionOp},
            withdraw::ChannelWithdrawOp,
        },
        transactions::{Ops, codec::encode_signed_mantle_tx},
    };
    use lb_groth16::{Fr, fr_to_bytes};
    use lb_key_management_system_service::keys::{ED25519_SECRET_KEY_SIZE, Ed25519Key, ZkKey};

    use crate::{
        cli::{WithdrawCombineArgs, WithdrawSignArgs},
        run_commands::{
            ZONE_FILE_TRANSFER_VERSION, ZONE_WITHDRAW_INTENT, ZONE_WITHDRAW_SIGNATURE,
            run_withdraw::{ZONE_WALLET_FUNDS_EXPORT, run_withdraw_combine, run_withdraw_sign},
            types::{
                AuthorizedSigner, ExportedUtxo, SignedWithdrawFile, WalletFundsExport,
                WithdrawFileEntry, WithdrawIntent, WithdrawSignatureFile,
            },
            utils::{
                build_deposit_op, build_deposit_transfer, decode_ed25519_public_key_hex,
                decode_exported_utxos, decode_hex, decode_hex_bincode, decode_mantle_tx_hex,
                decode_signed_mantle_tx_hex, decode_zk_public_key_hex, encode_hex_bincode,
                ensure_tx_hash, fixed_bytes, read_json, validate_kind, write_json,
            },
        },
    };

    fn test_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time must be after UNIX_EPOCH")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "tui-zone-manual-demo-test-{}-{nanos}-{name}",
            std::process::id()
        ))
    }

    fn test_zk_key() -> ZkKey {
        ZkKey::new(Fr::from(1u64))
    }

    fn test_utxo(value: Value, output_index: usize) -> Utxo {
        Utxo::new(
            [output_index as u8; 32],
            output_index,
            Note::new(value, test_zk_key().to_public_key()),
        )
    }

    fn test_signing_key(byte: u8) -> Ed25519Key {
        Ed25519Key::from_bytes(&[byte; ED25519_SECRET_KEY_SIZE])
    }

    fn empty_mantle_tx() -> MantleTx {
        MantleTx(Ops::try_from(Vec::new()).expect("empty ops must be valid"))
    }

    #[test]
    fn validate_kind_accepts_expected_versioned_kind() {
        validate_kind(ZONE_WALLET_FUNDS_EXPORT, ZONE_WALLET_FUNDS_EXPORT, 1).unwrap();
    }

    #[test]
    fn validate_kind_rejects_wrong_version_or_kind() {
        assert!(validate_kind(ZONE_WALLET_FUNDS_EXPORT, ZONE_WALLET_FUNDS_EXPORT, 2).is_err());
        assert!(validate_kind("other", ZONE_WALLET_FUNDS_EXPORT, 1).is_err());
    }

    #[test]
    fn hex_helpers_accept_prefixed_and_plain_values() {
        assert_eq!(decode_hex("0x0a0b").unwrap(), vec![10, 11]);
        assert_eq!(decode_hex("0a0b").unwrap(), vec![10, 11]);
        assert_eq!(fixed_bytes::<2>("0x0a0b").unwrap(), [10, 11]);
        assert!(fixed_bytes::<3>("0x0a0b").is_err());
    }

    #[test]
    fn bincode_hex_roundtrips_exported_utxo() {
        let utxo = test_utxo(42, 0);
        let encoded = encode_hex_bincode(&utxo).unwrap();
        let decoded = decode_hex_bincode::<Utxo>(&encoded).unwrap();
        assert_eq!(decoded, utxo);
    }

    #[test]
    fn public_key_decoders_accept_cli_json_hex_shapes() {
        let zk_public_key = test_zk_key().to_public_key();
        let zk_hex = hex::encode(fr_to_bytes(zk_public_key.as_fr()));
        assert_eq!(decode_zk_public_key_hex(&zk_hex).unwrap(), zk_public_key);

        let ed25519_public_key = test_signing_key(9).public_key();
        let ed25519_hex = hex::encode(ed25519_public_key.to_bytes());
        assert_eq!(
            decode_ed25519_public_key_hex(&ed25519_hex).unwrap(),
            ed25519_public_key
        );
    }

    #[test]
    fn mantle_tx_decoders_reject_trailing_bytes_and_hash_mismatches() {
        let tx = empty_mantle_tx();
        let encoded = hex::encode(tx.encode());
        assert_eq!(decode_mantle_tx_hex(&encoded).unwrap(), tx);
        assert!(decode_mantle_tx_hex(&format!("{encoded}00")).is_err());

        ensure_tx_hash(&hex::encode(tx.hash().as_ref()), tx.hash()).unwrap();
        assert!(ensure_tx_hash(&hex::encode([1u8; 32]), tx.hash()).is_err());

        let signed_tx = SignedMantleTx::new(tx, Vec::new()).unwrap();
        let signed_encoded = hex::encode(encode_signed_mantle_tx(&signed_tx));
        assert_eq!(
            decode_signed_mantle_tx_hex(&signed_encoded).unwrap(),
            signed_tx
        );
        assert!(decode_signed_mantle_tx_hex(&format!("{signed_encoded}00")).is_err());
    }

    #[test]
    fn decode_exported_utxos_reads_cucumber_wallet_funds_json_entries() {
        let utxos = vec![test_utxo(100, 0), test_utxo(200, 1)];
        let funds = WalletFundsExport {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WALLET_FUNDS_EXPORT.to_owned(),
            wallet: "WALLET_1A".to_owned(),
            node_url: "http://localhost:8080".to_owned(),
            public_key: hex::encode(fr_to_bytes(test_zk_key().to_public_key().as_fr())),
            secret_key: Some(encode_hex_bincode(&test_zk_key()).unwrap()),
            requested_value: 100,
            selected_value: 300,
            utxos: utxos
                .iter()
                .map(|utxo| ExportedUtxo {
                    utxo_id: hex::encode(utxo.id().as_bytes()),
                    value: utxo.note.value,
                    encoded_utxo: encode_hex_bincode(utxo).unwrap(),
                })
                .collect(),
        };

        assert_eq!(decode_exported_utxos(&funds).unwrap(), utxos);
    }

    #[test]
    fn build_deposit_transfer_selects_largest_inputs_and_returns_change() {
        let public_key = test_zk_key().to_public_key();
        let (transfer, selected) =
            build_deposit_transfer(vec![test_utxo(4, 0), test_utxo(10, 1)], public_key, 7).unwrap();

        assert_eq!(selected, vec![test_utxo(10, 1)]);
        let outputs = transfer.outputs.utxos(&transfer).collect::<Vec<_>>();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].note.value, 7);
        assert_eq!(outputs[1].note.value, 3);
    }

    #[test]
    fn build_deposit_transfer_rejects_insufficient_funds() {
        assert!(
            build_deposit_transfer(vec![test_utxo(4, 0)], test_zk_key().to_public_key(), 7)
                .is_err()
        );
    }

    #[test]
    fn build_deposit_op_consumes_first_transfer_output() {
        let (transfer, _) =
            build_deposit_transfer(vec![test_utxo(10, 1)], test_zk_key().to_public_key(), 7)
                .unwrap();
        let channel_id = ChannelId::from([5; 32]);
        let deposit = build_deposit_op(channel_id, &transfer, "demo metadata").unwrap();

        assert_eq!(deposit.channel_id, channel_id);
        assert_eq!(
            deposit.inputs,
            Inputs::new([transfer.outputs.utxo_by_index(0, &transfer).unwrap().id()])
        );
    }

    #[test]
    fn json_helpers_roundtrip_and_create_parent_directories() {
        let path = test_path("nested/funds.json");
        let funds = WalletFundsExport {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WALLET_FUNDS_EXPORT.to_owned(),
            wallet: "WALLET_1A".to_owned(),
            node_url: "http://localhost:8080".to_owned(),
            public_key: hex::encode(fr_to_bytes(test_zk_key().to_public_key().as_fr())),
            secret_key: None,
            requested_value: 0,
            selected_value: 0,
            utxos: Vec::new(),
        };

        write_json(&path, &funds).unwrap();
        let decoded = read_json::<WalletFundsExport>(&path).unwrap();
        assert_eq!(decoded.kind, funds.kind);
        assert_eq!(decoded.wallet, funds.wallet);

        drop(fs::remove_file(&path));
        if let Some(parent) = path.parent() {
            drop(fs::remove_dir_all(parent));
        }
    }

    #[test]
    fn withdraw_sign_and_combine_build_signed_withdraw_file_offline() {
        let channel_id = ChannelId::from([7; 32]);
        let signer = test_signing_key(2);
        let inscriber = test_signing_key(3);
        let recipient = test_zk_key().to_public_key();
        let withdraw = ChannelWithdrawOp {
            channel_id,
            outputs: Outputs::new([Note::new(500, recipient)]),
            withdraw_nonce: 0,
        };
        let inscribe = InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"withdraw wallet 1a".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: inscriber.public_key(),
        };
        let tx = MantleTx(
            Ops::try_from(vec![
                Op::ChannelWithdraw(withdraw),
                Op::ChannelInscribe(inscribe),
            ])
            .unwrap(),
        );
        let tx_hash = tx.hash();
        let msg_id = MsgId::from([8; 32]);
        let intent = WithdrawIntent {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WITHDRAW_INTENT.to_owned(),
            channel_id: hex::encode(channel_id.as_ref()),
            tx_hash: hex::encode(tx_hash.as_ref()),
            msg_id: hex::encode(msg_id.as_ref()),
            required_threshold: 1,
            mantle_tx: hex::encode(tx.encode()),
            inscription_signature: encode_hex_bincode(
                &inscriber.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )
            .unwrap(),
            withdraws: vec![WithdrawFileEntry {
                amount: 500,
                recipient_public_key: hex::encode(fr_to_bytes(recipient.as_fr())),
                withdraw_nonce: 0,
            }],
            authorized_signers: vec![AuthorizedSigner {
                key_index: 0,
                public_key: hex::encode(signer.public_key().to_bytes()),
            }],
            signatures: Vec::new(),
        };
        let key_path = test_path("signer.key");
        let intent_path = test_path("withdraw.intent.json");
        let sig_path = test_path("sig-a.json");
        let signed_path = test_path("withdraw.signed.json");

        fs::write(&key_path, [2u8; ED25519_SECRET_KEY_SIZE]).unwrap();
        write_json(&intent_path, &intent).unwrap();
        run_withdraw_sign(&WithdrawSignArgs {
            key_path: key_path.to_string_lossy().to_string(),
            input: intent_path.clone(),
            out: sig_path.clone(),
        })
        .unwrap();
        run_withdraw_combine(WithdrawCombineArgs {
            input: intent_path.clone(),
            sig: vec![sig_path.clone()],
            out: signed_path.clone(),
        })
        .unwrap();

        let sig_file = read_json::<WithdrawSignatureFile>(&sig_path).unwrap();
        assert_eq!(sig_file.signer_key_index, 0);
        assert_eq!(sig_file.tx_hash, intent.tx_hash);

        let signed_file = read_json::<SignedWithdrawFile>(&signed_path).unwrap();
        let signed_tx = decode_signed_mantle_tx_hex(&signed_file.signed_mantle_tx).unwrap();
        assert_eq!(signed_tx.hash(), tx_hash);
        assert_eq!(signed_file.signatures.len(), 1);

        drop(fs::remove_file(key_path));
        drop(fs::remove_file(intent_path));
        drop(fs::remove_file(sig_path));
        drop(fs::remove_file(signed_path));
    }

    #[test]
    fn withdraw_combine_rejects_too_few_signatures() {
        let tx = MantleTx(
            Ops::try_from(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id: ChannelId::from([9; 32]),
                outputs: Outputs::new([Note::new(1, test_zk_key().to_public_key())]),
                withdraw_nonce: 0,
            })])
            .unwrap(),
        );
        let intent = WithdrawIntent {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WITHDRAW_INTENT.to_owned(),
            channel_id: hex::encode([9; 32]),
            tx_hash: hex::encode(tx.hash().as_ref()),
            msg_id: hex::encode(MsgId::root().as_ref()),
            required_threshold: 1,
            mantle_tx: hex::encode(tx.encode()),
            inscription_signature: encode_hex_bincode(
                &test_signing_key(4).sign_payload(tx.hash().as_signing_bytes().as_ref()),
            )
            .unwrap(),
            withdraws: Vec::new(),
            authorized_signers: Vec::new(),
            signatures: Vec::new(),
        };
        let intent_path = test_path("withdraw-too-few.intent.json");
        let signed_path = test_path("withdraw-too-few.signed.json");

        write_json(&intent_path, &intent).unwrap();
        let error = run_withdraw_combine(WithdrawCombineArgs {
            input: intent_path.clone(),
            sig: Vec::new(),
            out: signed_path.clone(),
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("requires 1 unique authorized signature(s), got 0"));

        drop(fs::remove_file(intent_path));
        drop(fs::remove_file(signed_path));
    }

    #[test]
    fn withdraw_combine_counts_unique_signatures_after_proof_normalization() {
        let channel_id = ChannelId::from([10; 32]);
        let signer = test_signing_key(2);
        let second_signer = test_signing_key(3);
        let tx = MantleTx(
            Ops::try_from(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id,
                outputs: Outputs::new([Note::new(1, test_zk_key().to_public_key())]),
                withdraw_nonce: 0,
            })])
            .unwrap(),
        );
        let tx_hash = tx.hash();
        let intent = WithdrawIntent {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WITHDRAW_INTENT.to_owned(),
            channel_id: hex::encode(channel_id.as_ref()),
            tx_hash: hex::encode(tx_hash.as_ref()),
            msg_id: hex::encode(MsgId::root().as_ref()),
            required_threshold: 2,
            mantle_tx: hex::encode(tx.encode()),
            inscription_signature: encode_hex_bincode(
                &signer.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )
            .unwrap(),
            withdraws: Vec::new(),
            authorized_signers: vec![
                AuthorizedSigner {
                    key_index: 0,
                    public_key: hex::encode(signer.public_key().to_bytes()),
                },
                AuthorizedSigner {
                    key_index: 1,
                    public_key: hex::encode(second_signer.public_key().to_bytes()),
                },
            ],
            signatures: Vec::new(),
        };
        let sig = WithdrawSignatureFile {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WITHDRAW_SIGNATURE.to_owned(),
            channel_id: intent.channel_id.clone(),
            tx_hash: intent.tx_hash.clone(),
            signer_public_key: hex::encode(signer.public_key().to_bytes()),
            signer_key_index: 0,
            signature: encode_hex_bincode(
                &signer.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )
            .unwrap(),
        };
        let intent_path = test_path("withdraw-duplicate.intent.json");
        let sig_path = test_path("withdraw-duplicate.sig.json");
        let signed_path = test_path("withdraw-duplicate.signed.json");

        write_json(&intent_path, &intent).unwrap();
        write_json(&sig_path, &sig).unwrap();
        let error = run_withdraw_combine(WithdrawCombineArgs {
            input: intent_path.clone(),
            sig: vec![sig_path.clone(), sig_path.clone()],
            out: signed_path.clone(),
        })
        .unwrap_err()
        .to_string();

        assert!(
            error.contains("Signature indices are not strictly increasing"),
            "Error is: {error:?}",
        );

        drop(fs::remove_file(intent_path));
        drop(fs::remove_file(sig_path));
        drop(fs::remove_file(signed_path));
    }

    #[test]
    fn withdraw_combine_rejects_signature_with_mismatched_authorized_index() {
        let channel_id = ChannelId::from([11; 32]);
        let signer = test_signing_key(2);
        let second_signer = test_signing_key(3);
        let tx = MantleTx(
            Ops::try_from(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id,
                outputs: Outputs::new([Note::new(1, test_zk_key().to_public_key())]),
                withdraw_nonce: 0,
            })])
            .unwrap(),
        );
        let tx_hash = tx.hash();
        let intent = WithdrawIntent {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WITHDRAW_INTENT.to_owned(),
            channel_id: hex::encode(channel_id.as_ref()),
            tx_hash: hex::encode(tx_hash.as_ref()),
            msg_id: hex::encode(MsgId::root().as_ref()),
            required_threshold: 1,
            mantle_tx: hex::encode(tx.encode()),
            inscription_signature: encode_hex_bincode(
                &signer.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )
            .unwrap(),
            withdraws: Vec::new(),
            authorized_signers: vec![
                AuthorizedSigner {
                    key_index: 0,
                    public_key: hex::encode(signer.public_key().to_bytes()),
                },
                AuthorizedSigner {
                    key_index: 1,
                    public_key: hex::encode(second_signer.public_key().to_bytes()),
                },
            ],
            signatures: Vec::new(),
        };
        let sig = WithdrawSignatureFile {
            version: ZONE_FILE_TRANSFER_VERSION,
            kind: ZONE_WITHDRAW_SIGNATURE.to_owned(),
            channel_id: intent.channel_id.clone(),
            tx_hash: intent.tx_hash.clone(),
            signer_public_key: hex::encode(signer.public_key().to_bytes()),
            signer_key_index: 1,
            signature: encode_hex_bincode(
                &signer.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )
            .unwrap(),
        };
        let intent_path = test_path("withdraw-mismatched-index.intent.json");
        let sig_path = test_path("withdraw-mismatched-index.sig.json");
        let signed_path = test_path("withdraw-mismatched-index.signed.json");

        write_json(&intent_path, &intent).unwrap();
        write_json(&sig_path, &sig).unwrap();
        let error = run_withdraw_combine(WithdrawCombineArgs {
            input: intent_path.clone(),
            sig: vec![sig_path.clone()],
            out: signed_path.clone(),
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("is not listed in withdraw intent authorized_signers"));

        drop(fs::remove_file(intent_path));
        drop(fs::remove_file(sig_path));
        drop(fs::remove_file(signed_path));
    }
}
