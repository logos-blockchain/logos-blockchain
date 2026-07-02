use std::path::PathBuf;

use lb_core::{
    mantle::{
        Op, OpProof, SignedMantleTx, Transaction as _,
        ops::channel::{
            ChannelId, ChannelKeyIndex,
            config::{ChannelConfigOp, Keys},
        },
        transactions::codec::{encode_mantle_tx, encode_signed_mantle_tx},
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_key_management_system_service::keys::{Ed25519PublicKey, Ed25519Signature};

use crate::{
    cli::{
        ConfigArgs, ConfigCombineArgs, ConfigPrepareArgs, ConfigSignArgs, ConfigSubmitArgs,
        RunResult,
    },
    run_commands::{
        ZONE_CONFIG_INTENT, ZONE_CONFIG_SIGNATURE, ZONE_FILE_TRANSFER_VERSION, ZONE_SIGNED_CONFIG,
        driver::{CommandGoal, WaitFor, drive_until_observed},
        types::{
            AuthorizedSigner, ConfigIntent, ConfigSignatureFile, SignedConfigFile,
            WithdrawSignatureEntry,
        },
        utils::{
            decode_ed25519_public_key_hex, decode_hex_bincode, decode_mantle_tx_hex,
            decode_msg_id_hex, decode_signed_mantle_tx_hex, encode_hex_bincode, ensure_tx_hash,
            fixed_bytes, load_or_create_signing_key, node_client, print_channel_state,
            query_channel_state, read_json, resolve_channel_id,
            start_cli_sequencer_with_channel_state, timestamp, validate_kind, write_json,
        },
    },
};

pub(crate) async fn run_config(args: ConfigArgs) -> RunResult<()> {
    let authorized_keys =
        authorized_keys_for_paths(&args.node_key.key_path, &args.authorized_key_paths);
    validate_config_thresholds(
        args.configuration_threshold,
        args.withdraw_threshold,
        authorized_keys.len(),
    )?;

    let channel_id = resolve_channel_id(&args.node_key)?;
    let (mut sequencer, channel_state) =
        start_cli_sequencer_with_channel_state(&args.node_key).await?;
    print_channel_state("zone_config before", &channel_id, channel_state.as_ref());
    let status_rx = sequencer.subscribe_tx_status();
    let (_result, _checkpoint, signed_tx) = sequencer.handle().channel_config(
        Keys::try_from(authorized_keys)?,
        args.posting_timeframe.into(),
        args.posting_timeout.into(),
        args.configuration_threshold,
        args.withdraw_threshold,
    )?;
    let tx_hash = signed_tx.hash();
    let goal = CommandGoal::Tx { tx_hash };
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
        "zone_config",
    )
    .await?;
    let node = node_client(&args.node_key.node_url)?;
    let channel_state = query_channel_state(&node, channel_id).await;
    print_channel_state("zone_config after", &channel_id, channel_state.as_ref());
    Ok(())
}

pub(crate) async fn run_config_prepare(args: ConfigPrepareArgs) -> RunResult<()> {
    let authorized_keys =
        authorized_keys_for_paths(&args.node_key.key_path, &args.authorized_key_paths);
    validate_config_thresholds(
        args.configuration_threshold,
        args.withdraw_threshold,
        authorized_keys.len(),
    )?;
    let channel_id = resolve_channel_id(&args.node_key)?;
    let (_sequencer, channel_state) =
        start_cli_sequencer_with_channel_state(&args.node_key).await?;
    let channel_state =
        channel_state.ok_or_else(|| format!("channel state not found for {channel_id}"))?;
    let config_op = build_config_op(
        channel_id,
        authorized_keys.clone(),
        args.posting_timeframe,
        args.posting_timeout,
        args.configuration_threshold,
        args.withdraw_threshold,
    )?;
    let msg_id = config_op.id();
    let tx = lb_core::mantle::MantleTx([Op::ChannelConfig(config_op)].into());
    let tx_hash = tx.hash();
    let intent = ConfigIntent {
        version: ZONE_FILE_TRANSFER_VERSION,
        kind: ZONE_CONFIG_INTENT.to_owned(),
        channel_id: hex::encode(channel_id.as_ref()),
        tx_hash: hex::encode(tx_hash.as_ref()),
        msg_id: hex::encode(msg_id.as_ref()),
        required_threshold: channel_state.configuration_threshold,
        mantle_tx: hex::encode(encode_mantle_tx(&tx)),
        new_authorized_keys: authorized_keys
            .iter()
            .map(|key| hex::encode(key.to_bytes()))
            .collect(),
        configuration_threshold: args.configuration_threshold,
        withdraw_threshold: args.withdraw_threshold,
        posting_timeframe: args.posting_timeframe,
        posting_timeout: args.posting_timeout,
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
        "{} zone_config: intent tx_hash={} msg_id={} required_threshold={}",
        timestamp(),
        intent.tx_hash,
        intent.msg_id,
        intent.required_threshold
    );
    Ok(())
}

pub(crate) fn run_config_sign(args: &ConfigSignArgs) -> RunResult<()> {
    let intent = read_json::<ConfigIntent>(&args.input)?;
    validate_kind(&intent.kind, ZONE_CONFIG_INTENT, intent.version)?;
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
            "signing key is not listed in config intent authorized_signers".to_owned()
        })?;
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());
    let signature_file = ConfigSignatureFile {
        version: ZONE_FILE_TRANSFER_VERSION,
        kind: ZONE_CONFIG_SIGNATURE.to_owned(),
        channel_id: intent.channel_id,
        tx_hash: intent.tx_hash,
        signer_public_key: signer.public_key.clone(),
        signer_key_index: signer.key_index,
        signature: encode_hex_bincode(&signature)?,
    };
    write_json(&args.out, &signature_file)?;
    println!(
        "{} zone_config: signature tx_hash={} signer_key_index={}",
        timestamp(),
        signature_file.tx_hash,
        signature_file.signer_key_index
    );
    Ok(())
}

pub(crate) fn run_config_combine(args: ConfigCombineArgs) -> RunResult<()> {
    let intent = read_json::<ConfigIntent>(&args.input)?;
    validate_kind(&intent.kind, ZONE_CONFIG_INTENT, intent.version)?;
    let tx = decode_mantle_tx_hex(&intent.mantle_tx)?;
    let tx_hash = tx.hash();
    ensure_tx_hash(&intent.tx_hash, tx_hash)?;
    validate_config_tx(&tx, &intent)?;
    let mut signature_entries = intent.signatures.clone();
    for path in args.sig {
        let sig = read_json::<ConfigSignatureFile>(&path)?;
        validate_kind(&sig.kind, ZONE_CONFIG_SIGNATURE, sig.version)?;
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
    let proof = ChannelMultiSigProof::try_new(
        signature_entries
            .iter()
            .map(|sig| {
                validate_authorized_signer(&intent, sig.signer_key_index, &sig.signer_public_key)?;
                decode_hex_bincode::<Ed25519Signature>(&sig.signature)
                    .map(|signature| IndexedSignature::new(sig.signer_key_index, signature))
            })
            .collect::<RunResult<Vec<_>>>()?
            .try_into()?,
    )?;
    if proof.signatures().len() != intent.required_threshold as usize {
        return Err(format!(
            "config combine requires exactly {} unique authorized signature(s), got {}",
            intent.required_threshold,
            proof.signatures().len()
        )
        .into());
    }
    let signed_tx = SignedMantleTx::new(tx, vec![OpProof::ChannelMultiSigProof(proof)])?;
    let signed = SignedConfigFile {
        version: ZONE_FILE_TRANSFER_VERSION,
        kind: ZONE_SIGNED_CONFIG.to_owned(),
        channel_id: intent.channel_id,
        tx_hash: intent.tx_hash,
        msg_id: intent.msg_id,
        signed_mantle_tx: hex::encode(encode_signed_mantle_tx(&signed_tx)),
        signatures: signature_entries,
    };
    write_json(&args.out, &signed)?;
    println!(
        "{} zone_config: signed tx_hash={} msg_id={}",
        timestamp(),
        signed.tx_hash,
        signed.msg_id
    );
    Ok(())
}

pub(crate) async fn run_config_submit(args: ConfigSubmitArgs) -> RunResult<()> {
    let signed = read_json::<SignedConfigFile>(&args.input)?;
    validate_kind(&signed.kind, ZONE_SIGNED_CONFIG, signed.version)?;
    let signed_tx = decode_signed_mantle_tx_hex(&signed.signed_mantle_tx)?;
    let tx_hash = signed_tx.hash();
    ensure_tx_hash(&signed.tx_hash, tx_hash)?;
    let channel_id = ChannelId::from(fixed_bytes::<32>(&signed.channel_id)?);
    if let Some(requested_channel_id) = args.node_key.channel_id.as_deref()
        && signed.channel_id != requested_channel_id
    {
        return Err(format!(
            "signed config channel_id {} does not match requested channel_id {}",
            signed.channel_id, requested_channel_id
        )
        .into());
    }
    let mut node_key = args.node_key;
    node_key.channel_id = Some(signed.channel_id.clone());
    let (mut sequencer, channel_state) = start_cli_sequencer_with_channel_state(&node_key).await?;
    print_channel_state("zone_config before", &channel_id, channel_state.as_ref());
    let status_rx = sequencer.subscribe_tx_status();
    let goal = CommandGoal::Tx { tx_hash };
    let (_result, _checkpoint) = sequencer
        .handle()
        .submit_signed_tx(signed_tx, decode_msg_id_hex(&signed.msg_id)?)?;
    println!(
        "{} zone_config: submitted tx_hash={} msg_id={}",
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
        "zone_config",
    )
    .await?;
    let node = node_client(&node_key.node_url)?;
    let channel_state = query_channel_state(&node, channel_id).await;
    print_channel_state("zone_config after", &channel_id, channel_state.as_ref());
    Ok(())
}

fn validate_config_thresholds(
    configuration_threshold: u16,
    withdraw_threshold: u16,
    authorized_key_count: usize,
) -> RunResult<()> {
    if withdraw_threshold == 0 {
        return Err("withdraw_threshold must be greater than 0".into());
    }
    if configuration_threshold == 0 {
        return Err("configuration_threshold must be greater than 0".into());
    }
    if withdraw_threshold as usize > authorized_key_count {
        return Err(format!(
            "withdraw_threshold {withdraw_threshold} exceeds authorized key count {authorized_key_count}"
        )
        .into());
    }
    if configuration_threshold as usize > authorized_key_count {
        return Err(format!(
            "configuration_threshold {configuration_threshold} exceeds authorized key count {authorized_key_count}"
        )
        .into());
    }
    Ok(())
}

fn authorized_keys_for_paths(
    admin_key_path: &str,
    authorized_key_paths: &[String],
) -> Vec<Ed25519PublicKey> {
    let admin_key = load_or_create_signing_key(PathBuf::from(admin_key_path).as_path());
    let mut authorized_keys = vec![admin_key.public_key()];
    for key_path in authorized_key_paths {
        let public_key = load_or_create_signing_key(PathBuf::from(key_path).as_path()).public_key();
        if !authorized_keys.contains(&public_key) {
            authorized_keys.push(public_key);
        }
    }
    authorized_keys
}

fn build_config_op(
    channel_id: ChannelId,
    authorized_keys: Vec<Ed25519PublicKey>,
    posting_timeframe: u32,
    posting_timeout: u32,
    configuration_threshold: u16,
    withdraw_threshold: u16,
) -> RunResult<ChannelConfigOp> {
    Ok(ChannelConfigOp {
        channel: channel_id,
        keys: Keys::try_from(authorized_keys)?,
        posting_timeframe: posting_timeframe.into(),
        posting_timeout: posting_timeout.into(),
        configuration_threshold,
        withdraw_threshold,
    })
}

fn validate_authorized_signer(
    intent: &ConfigIntent,
    signer_key_index: ChannelKeyIndex,
    signer_public_key: &str,
) -> RunResult<()> {
    if intent.authorized_signers.iter().any(|signer| {
        signer.key_index == signer_key_index && signer.public_key == signer_public_key
    }) {
        return Ok(());
    }
    Err(format!(
        "signature signer key_index={signer_key_index} public_key={signer_public_key} is not listed in config intent authorized_signers"
    )
    .into())
}

fn validate_config_tx(tx: &lb_core::mantle::MantleTx, intent: &ConfigIntent) -> RunResult<()> {
    let config = tx
        .ops()
        .iter()
        .find_map(|op| match op {
            Op::ChannelConfig(config) => Some(config),
            _ => None,
        })
        .ok_or("config intent transaction has no ChannelConfig op")?;
    if hex::encode(config.id().as_ref()) != intent.msg_id {
        return Err("config intent msg_id does not match ChannelConfig op id".into());
    }
    if tx.ops().len() != 1 {
        return Err("config intent transaction must contain exactly one op".into());
    }
    Ok(())
}
