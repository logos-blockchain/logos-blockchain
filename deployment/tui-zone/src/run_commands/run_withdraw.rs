use std::path::PathBuf;

use lb_core::{
    mantle::{
        Note, Op, OpProof, SignedMantleTx, Transaction as _,
        encoding::{encode_mantle_tx, encode_signed_mantle_tx},
        ledger::Outputs,
        ops::channel::{
            ChannelId, ChannelKeyIndex, inscribe::Inscription, withdraw::ChannelWithdrawOp,
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_key_management_system_service::keys::Ed25519Signature;

pub(crate) use crate::{
    cli::{
        RunResult, WithdrawCombineArgs, WithdrawPrepareArgs, WithdrawSignArgs, WithdrawSubmitArgs,
    },
    run_commands::{
        ZONE_FILE_TRANSFER_VERSION, ZONE_SIGNED_TRANSACTION, ZONE_WALLET_FUNDS_EXPORT,
        ZONE_WITHDRAW_INTENT, ZONE_WITHDRAW_SIGNATURE,
        driver::{CommandGoal, WaitFor, drive_until_observed},
        types::{
            AuthorizedSigner, SignedWithdrawFile, WalletFundsExport, WithdrawFileEntry,
            WithdrawIntent, WithdrawSignatureEntry, WithdrawSignatureFile,
        },
        utils::{
            decode_ed25519_public_key_hex, decode_hex_bincode, decode_mantle_tx_hex,
            decode_msg_id_hex, decode_signed_mantle_tx_hex, decode_zk_public_key_hex,
            encode_hex_bincode, ensure_tx_hash, fixed_bytes, load_or_create_signing_key,
            node_client, print_channel_balance, query_channel_state, read_json, resolve_channel_id,
            start_cli_sequencer, start_cli_sequencer_with_channel_state, timestamp, validate_kind,
            write_json,
        },
    },
};

pub(crate) async fn run_withdraw_prepare(args: WithdrawPrepareArgs) -> RunResult<()> {
    let funds = read_json::<WalletFundsExport>(&args.recipient_funds)?;
    validate_kind(&funds.kind, ZONE_WALLET_FUNDS_EXPORT, funds.version)?;
    let recipient = decode_zk_public_key_hex(&funds.public_key)?;
    let channel_id = resolve_channel_id(&args.node_key)?;
    let (mut sequencer, channel_state) =
        start_cli_sequencer_with_channel_state(&args.node_key).await?;
    let channel_state =
        channel_state.ok_or_else(|| format!("channel state not found for {channel_id}"))?;
    print_channel_balance("withdraw before", &channel_id, Some(&channel_state));
    if args.amount > channel_state.balance {
        return Err(format!(
            "insufficient channel balance for withdraw: requested {}, available {}",
            args.amount, channel_state.balance
        )
        .into());
    }
    let withdraw_nonce = channel_state.withdrawal_nonce;
    let withdraw = ChannelWithdrawOp {
        channel_id,
        outputs: Outputs::new([Note::new(args.amount, recipient)]),
        withdraw_nonce,
    };
    let inscription = Inscription::try_from(args.message.into_bytes())?;
    let (tx, msg_id, inscription_signature) = sequencer
        .handle()
        .prepare_tx([Op::ChannelWithdraw(withdraw)].into(), inscription)?;
    let tx_hash = tx.hash();
    let intent = WithdrawIntent {
        version: ZONE_FILE_TRANSFER_VERSION,
        kind: ZONE_WITHDRAW_INTENT.to_owned(),
        channel_id: hex::encode(channel_id.as_ref()),
        tx_hash: hex::encode(tx_hash.as_ref()),
        msg_id: hex::encode(msg_id.as_ref()),
        required_threshold: channel_state.withdraw_threshold,
        mantle_tx: hex::encode(encode_mantle_tx(&tx)),
        inscription_signature: encode_hex_bincode(&inscription_signature)?,
        withdraws: vec![WithdrawFileEntry {
            amount: args.amount,
            recipient_public_key: funds.public_key,
            withdraw_nonce,
        }],
        authorized_signers: channel_state
            .accredited_keys
            .iter()
            .enumerate()
            .map(|(index, key)| AuthorizedSigner {
                key_index: index as ChannelKeyIndex,
                public_key: hex::encode(key.to_bytes()),
            })
            .collect(),
        signatures: Vec::new(),
    };
    write_json(&args.out, &intent)?;
    println!(
        "{} withdraw: intent tx_hash={} msg_id={}",
        timestamp(),
        intent.tx_hash,
        intent.msg_id
    );
    Ok(())
}

pub(crate) fn run_withdraw_sign(args: &WithdrawSignArgs) -> RunResult<()> {
    let intent = read_json::<WithdrawIntent>(&args.input)?;
    validate_kind(&intent.kind, ZONE_WITHDRAW_INTENT, intent.version)?;
    let tx = decode_mantle_tx_hex(&intent.mantle_tx)?;
    let tx_hash = tx.hash();
    ensure_tx_hash(&intent.tx_hash, tx_hash)?;
    let signing_key = load_or_create_signing_key(PathBuf::from(&args.key_path).as_path());
    let public_key = signing_key.public_key();
    let signer = intent
        .authorized_signers
        .iter()
        .find(|signer| signer.public_key == hex::encode(public_key.to_bytes()))
        .ok_or_else(|| {
            "signing key is not listed in withdraw intent authorized_signers".to_owned()
        })?;
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());
    let signature_file = WithdrawSignatureFile {
        version: ZONE_FILE_TRANSFER_VERSION,
        kind: ZONE_WITHDRAW_SIGNATURE.to_owned(),
        channel_id: intent.channel_id,
        tx_hash: intent.tx_hash,
        signer_public_key: signer.public_key.clone(),
        signer_key_index: signer.key_index,
        signature: encode_hex_bincode(&signature)?,
    };
    write_json(&args.out, &signature_file)?;
    println!(
        "{} withdraw: signature tx_hash={} signer_key_index={}",
        timestamp(),
        signature_file.tx_hash,
        signature_file.signer_key_index
    );
    Ok(())
}

pub(crate) fn run_withdraw_combine(args: WithdrawCombineArgs) -> RunResult<()> {
    let intent = read_json::<WithdrawIntent>(&args.input)?;
    validate_kind(&intent.kind, ZONE_WITHDRAW_INTENT, intent.version)?;
    let tx = decode_mantle_tx_hex(&intent.mantle_tx)?;
    let tx_hash = tx.hash();
    ensure_tx_hash(&intent.tx_hash, tx_hash)?;
    let mut signature_entries = intent.signatures.clone();
    for path in args.sig {
        let sig = read_json::<WithdrawSignatureFile>(&path)?;
        validate_kind(&sig.kind, ZONE_WITHDRAW_SIGNATURE, sig.version)?;
        if sig.channel_id != intent.channel_id {
            return Err(format!(
                "signature '{}' channel_id {} does not match intent {}",
                path.display(),
                sig.channel_id,
                intent.channel_id
            )
            .into());
        }
        if sig.tx_hash != intent.tx_hash {
            return Err(format!(
                "signature '{}' tx_hash {} does not match intent {}",
                path.display(),
                sig.tx_hash,
                intent.tx_hash
            )
            .into());
        }
        let public_key = decode_ed25519_public_key_hex(&sig.signer_public_key)?;
        validate_authorized_signer(&intent, sig.signer_key_index, &sig.signer_public_key)?;
        let signature = decode_hex_bincode::<Ed25519Signature>(&sig.signature)?;
        public_key.verify(tx_hash.as_signing_bytes().as_ref(), &signature)?;
        signature_entries.push(WithdrawSignatureEntry {
            signer_key_index: sig.signer_key_index,
            signer_public_key: sig.signer_public_key,
            signature: sig.signature,
        });
    }
    let proof = ChannelMultiSigProof::new(
        signature_entries
            .iter()
            .map(|sig| {
                validate_authorized_signer(&intent, sig.signer_key_index, &sig.signer_public_key)?;
                decode_hex_bincode::<Ed25519Signature>(&sig.signature)
                    .map(|signature| IndexedSignature::new(sig.signer_key_index, signature))
            })
            .collect::<RunResult<Vec<_>>>()?,
    )?;
    if proof.signatures().len() < intent.required_threshold as usize {
        return Err(format!(
            "withdraw combine requires {} unique authorized signature(s), got {}",
            intent.required_threshold,
            proof.signatures().len()
        )
        .into());
    }
    let inscription_signature =
        decode_hex_bincode::<Ed25519Signature>(&intent.inscription_signature)?;
    let op_proofs = tx
        .ops()
        .iter()
        .map(|op| match op {
            Op::ChannelWithdraw(_) => Ok(OpProof::ChannelMultiSigProof(proof.clone())),
            Op::ChannelInscribe(_) => Ok(OpProof::Ed25519Sig(inscription_signature)),
            other => Err(format!("unexpected op in withdraw intent: {other:?}").into()),
        })
        .collect::<RunResult<Vec<_>>>()?;
    let signed_tx = SignedMantleTx::new(tx, op_proofs)?;
    let signed = SignedWithdrawFile {
        version: ZONE_FILE_TRANSFER_VERSION,
        kind: ZONE_SIGNED_TRANSACTION.to_owned(),
        channel_id: intent.channel_id,
        tx_hash: intent.tx_hash,
        msg_id: intent.msg_id,
        signed_mantle_tx: hex::encode(encode_signed_mantle_tx(&signed_tx)),
        signatures: signature_entries,
    };
    write_json(&args.out, &signed)?;
    println!(
        "{} withdraw: signed tx_hash={} msg_id={}",
        timestamp(),
        signed.tx_hash,
        signed.msg_id
    );
    Ok(())
}

fn validate_authorized_signer(
    intent: &WithdrawIntent,
    signer_key_index: ChannelKeyIndex,
    signer_public_key: &str,
) -> RunResult<()> {
    if intent.authorized_signers.iter().any(|signer| {
        signer.key_index == signer_key_index && signer.public_key == signer_public_key
    }) {
        return Ok(());
    }
    Err(format!(
        "signature signer key_index={signer_key_index} public_key={signer_public_key} is not listed in withdraw intent authorized_signers"
    )
    .into())
}

fn decode_channel_id_hex(channel_id: &str) -> RunResult<ChannelId> {
    Ok(ChannelId::from(fixed_bytes::<32>(channel_id)?))
}

pub(crate) async fn run_withdraw_submit(args: WithdrawSubmitArgs) -> RunResult<()> {
    let signed = read_json::<SignedWithdrawFile>(&args.input)?;
    validate_kind(&signed.kind, ZONE_SIGNED_TRANSACTION, signed.version)?;
    let signed_tx = decode_signed_mantle_tx_hex(&signed.signed_mantle_tx)?;
    let tx_hash = signed_tx.hash();
    ensure_tx_hash(&signed.tx_hash, tx_hash)?;
    let channel_id = decode_channel_id_hex(&signed.channel_id)?;
    if let Some(requested_channel_id) = args.node_key.channel_id.as_deref()
        && signed.channel_id != requested_channel_id
    {
        return Err(format!(
            "signed withdraw channel_id {} does not match requested channel_id {}",
            signed.channel_id, requested_channel_id
        )
        .into());
    }
    let mut node_key = args.node_key;
    node_key.channel_id = Some(signed.channel_id.clone());
    let tx = signed_tx.mantle_tx.clone();
    let withdraws = tx
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::ChannelWithdraw(withdraw) => Some(withdraw.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if withdraws.is_empty() {
        return Err("signed withdraw transaction has no withdraw ops".into());
    }
    if withdraws
        .iter()
        .any(|withdraw| withdraw.channel_id != channel_id)
    {
        return Err(
            "signed withdraw transaction channel_id does not match requested channel".into(),
        );
    }
    let goal = CommandGoal::Withdraw { tx_hash, withdraws };
    let mut sequencer = start_cli_sequencer(&node_key).await?;
    let status_rx = sequencer.subscribe_tx_status();
    let (_result, _checkpoint) = sequencer
        .handle()
        .submit_signed_tx(signed_tx, decode_msg_id_hex(&signed.msg_id)?)?;
    println!(
        "{} withdraw: submitted tx_hash={} msg_id={}",
        timestamp(),
        signed.tx_hash,
        signed.msg_id
    );
    let wait_for = if args.wait_finalized {
        WaitFor::Finalized
    } else {
        WaitFor::OnChain
    };
    drive_until_observed(
        &channel_id,
        &mut sequencer,
        status_rx,
        goal,
        wait_for,
        "withdraw",
    )
    .await?;
    let node = node_client(&node_key.node_url)?;
    let channel_state = query_channel_state(&node, channel_id).await;
    print_channel_balance("withdraw after", &channel_id, channel_state.as_ref());
    Ok(())
}
