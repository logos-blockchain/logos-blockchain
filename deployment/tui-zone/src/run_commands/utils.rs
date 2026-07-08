use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use lb_core::mantle::{
    MantleTx, Note, SignedMantleTx, TxHash, Utxo, Value,
    channel::ChannelState,
    ledger::{Inputs, Outputs},
    nom::NomDecode as _,
    ops::{
        channel::{
            ChannelId, MsgId,
            deposit::{DepositOp, Metadata},
        },
        transfer::TransferOp,
    },
    transactions::codec::decode_signed_mantle_tx,
};
use lb_key_management_system_service::keys::{
    ED25519_SECRET_KEY_SIZE, Ed25519Key, Ed25519PublicKey, ZkPublicKey,
};
use lb_zone_sdk::{
    CommonHttpClient,
    adapter::{Node as _, NodeHttpClient},
    sequencer::{Event, SequencerCheckpoint, ZoneSequencer},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::warn;

use crate::{
    cli::{NodeKeyArgs, RunResult},
    run_commands::types::WalletFundsExport,
    state::{load_or_discard_persisted_checkpoint_for_channel, save_persisted_checkpoint},
};

const CHANNEL_STATE_QUERY_RETRY: Duration = Duration::from_secs(5);

/// Format the current wall-clock time as RFC 3339 / ISO-8601 UTC timestamp with
/// microseconds, example `2026-06-19T05:43:07.036408Z`.
pub fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let secs = now.as_secs();
    let micros = now.subsec_micros();

    let datetime = DateTime::<Utc>::from_timestamp(secs as i64, micros * 1_000)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());

    datetime.format("%Y-%m-%dT%H:%M:%S.%6fZ").to_string()
}

/// Load an Ed25519 signing key from disk or create a new one at `path`.
pub fn load_or_create_signing_key(path: &Path) -> Ed25519Key {
    if path.exists() {
        let key_bytes = fs::read(path).expect("failed to read key file");
        assert_eq!(
            key_bytes.len(),
            ED25519_SECRET_KEY_SIZE,
            "invalid key file: expected {} bytes, got {}",
            ED25519_SECRET_KEY_SIZE,
            key_bytes.len()
        );
        let key_array: [u8; ED25519_SECRET_KEY_SIZE] =
            key_bytes.try_into().expect("length already checked");
        Ed25519Key::from_bytes(&key_array)
    } else {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).expect("failed to create key file parent directory");
        }
        let mut key_bytes = [0u8; ED25519_SECRET_KEY_SIZE];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key_bytes);
        fs::write(path, key_bytes).expect("failed to write key file");
        Ed25519Key::from_bytes(&key_bytes)
    }
}

/// Build a node HTTP client from a URL string.
pub fn node_client(node_url: &str) -> RunResult<NodeHttpClient> {
    Ok(NodeHttpClient::new(
        CommonHttpClient::new(None),
        Url::parse(node_url)?,
    ))
}

/// Validate a versioned JSON file kind discriminator.
pub fn validate_kind(actual: &str, expected: &str, version: u8) -> RunResult<()> {
    if version != 1 {
        return Err(format!("unsupported {actual} version {version}; expected version 1").into());
    }
    if actual != expected {
        return Err(format!("unsupported JSON kind '{actual}'; expected '{expected}'").into());
    }
    Ok(())
}

/// Read and deserialize a JSON file.
pub fn read_json<T: for<'de> Deserialize<'de>>(path: &PathBuf) -> RunResult<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// Serialize and write a JSON file, creating parent directories when needed.
pub fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> RunResult<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

/// Decode a required hex-encoded bincode value.
pub fn decode_required_hex_bincode<T: for<'de> Deserialize<'de>>(
    value: Option<&str>,
    missing: &str,
) -> RunResult<T> {
    let value = value.ok_or_else(|| missing.to_owned())?;
    decode_hex_bincode(value)
}

/// Encode a serializable value as hex-encoded bincode.
pub fn encode_hex_bincode<T: Serialize>(value: &T) -> RunResult<String> {
    Ok(hex::encode(bincode::serialize(value)?))
}

/// Decode a hex-encoded bincode value.
pub fn decode_hex_bincode<T: for<'de> Deserialize<'de>>(value: &str) -> RunResult<T> {
    Ok(bincode::deserialize(&decode_hex(value)?)?)
}

/// Decode a plain or `0x`-prefixed hex string.
pub fn decode_hex(value: &str) -> RunResult<Vec<u8>> {
    Ok(hex::decode(value.strip_prefix("0x").unwrap_or(value))?)
}

/// Derive the zone channel ID from a channel signing key path.
pub fn channel_id_for_key_path(path: &str) -> ChannelId {
    let signing_key = load_or_create_signing_key(PathBuf::from(path).as_path());
    ChannelId::from(signing_key.public_key().to_bytes())
}

/// Resolve the target channel ID from CLI args or the signing key path.
pub fn resolve_channel_id(args: &NodeKeyArgs) -> RunResult<ChannelId> {
    args.channel_id.as_ref().map_or_else(
        || Ok(channel_id_for_key_path(&args.key_path)),
        |channel_id| Ok(ChannelId::from(fixed_bytes::<32>(channel_id)?)),
    )
}

/// Query whether the node has channel state, retrying until the node responds.
pub async fn query_channel_exists(node: &NodeHttpClient, channel_id: ChannelId) -> bool {
    query_channel_state(node, channel_id).await.is_some()
}

/// Query channel state, retrying until the node responds.
pub async fn query_channel_state(
    node: &NodeHttpClient,
    channel_id: ChannelId,
) -> Option<ChannelState> {
    loop {
        match node.channel_state(channel_id).await {
            Ok(channel_state) => return channel_state,
            Err(error) => {
                warn!("failed to query channel state before sequencer init: {error}");
                sleep(CHANNEL_STATE_QUERY_RETRY).await;
            }
        }
    }
}

/// Print the channel's current balance.
pub fn print_channel_balance(label: &str, channel_id: &ChannelId, state: Option<&ChannelState>) {
    match state {
        Some(state) => println!(
            "{} {label}: channel_id={} balance={}",
            timestamp(),
            hex::encode(channel_id.as_ref()),
            state.balance,
        ),
        None => println!(
            "{} {label}: channel_id={} balance=unknown channel_state=missing",
            timestamp(),
            hex::encode(channel_id.as_ref())
        ),
    }
}

/// Print the channel's current balance and configuration state.
pub fn print_channel_state(label: &str, channel_id: &ChannelId, state: Option<&ChannelState>) {
    match state {
        Some(state) => println!(
            "{} {label}: channel_id={} balance={} withdrawal_nonce={} accredited_keys={} configuration_threshold={} withdraw_threshold={} tip_message={}",
            timestamp(),
            hex::encode(channel_id.as_ref()),
            state.balance,
            state.withdrawal_nonce,
            state.accredited_keys.len(),
            state.configuration_threshold,
            state.withdraw_threshold,
            hex::encode(state.tip_message.as_ref())
        ),
        None => println!(
            "{} {label}: channel_id={} balance=unknown channel_state=missing",
            timestamp(),
            hex::encode(channel_id.as_ref())
        ),
    }
}

/// Decode all UTXOs embedded in a wallet funds export.
pub fn decode_exported_utxos(funds: &WalletFundsExport) -> RunResult<Vec<Utxo>> {
    funds
        .utxos
        .iter()
        .map(|utxo| decode_hex_bincode::<Utxo>(&utxo.encoded_utxo))
        .collect()
}

/// Decode a hex-encoded mantle transaction and reject trailing bytes.
pub fn decode_mantle_tx_hex(value: &str) -> RunResult<MantleTx> {
    let bytes = decode_hex(value)?;
    let (remaining, tx) = MantleTx::decode(&bytes).map_err(|error| format!("{error:?}"))?;
    if !remaining.is_empty() {
        return Err("mantle tx has trailing bytes".into());
    }
    Ok(tx)
}

/// Decode a hex-encoded signed mantle transaction and reject trailing bytes.
pub fn decode_signed_mantle_tx_hex(value: &str) -> RunResult<SignedMantleTx> {
    let bytes = decode_hex(value)?;
    let (remaining, tx) = decode_signed_mantle_tx(&bytes).map_err(|error| format!("{error:?}"))?;
    if !remaining.is_empty() {
        return Err("signed mantle tx has trailing bytes".into());
    }
    Ok(tx)
}

/// Decode a hex-encoded ZK public key.
pub fn decode_zk_public_key_hex(value: &str) -> RunResult<ZkPublicKey> {
    let bytes = fixed_bytes::<32>(value)?;
    Ok(ZkPublicKey::new(lb_groth16::fr_from_bytes(&bytes)?))
}

/// Decode a hex-encoded Ed25519 public key.
pub fn decode_ed25519_public_key_hex(value: &str) -> RunResult<Ed25519PublicKey> {
    let bytes = fixed_bytes::<32>(value)?;
    Ok(Ed25519PublicKey::from_bytes(&bytes)?)
}

/// Decode a hex-encoded zone message ID.
pub fn decode_msg_id_hex(value: &str) -> RunResult<MsgId> {
    Ok(MsgId::from(fixed_bytes(value)?))
}

/// Decode a hex string into exactly `N` bytes.
pub fn fixed_bytes<const N: usize>(value: &str) -> RunResult<[u8; N]> {
    let bytes = decode_hex(value)?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("expected {N} bytes, got {}", bytes.len()).into())
}

/// Ensure a decoded transaction hash matches the expected hex string.
pub fn ensure_tx_hash(expected_hex: &str, actual: TxHash) -> RunResult<()> {
    let actual_hex = hex::encode(actual.as_ref());
    if expected_hex != actual_hex {
        return Err(format!(
            "tx_hash mismatch: JSON has {expected_hex}, decoded tx has {actual_hex}"
        )
        .into());
    }
    Ok(())
}

/// Build a transfer op that selects exported UTXOs and returns change.
pub fn build_deposit_transfer(
    mut available_utxos: Vec<Utxo>,
    funding_public_key: ZkPublicKey,
    amount: Value,
) -> RunResult<(TransferOp, Vec<Utxo>)> {
    available_utxos.sort_by_key(|utxo| std::cmp::Reverse(utxo.note.value));
    let mut selected = Vec::new();
    let mut selected_value = 0u64;
    for utxo in available_utxos {
        selected_value = selected_value.saturating_add(utxo.note.value);
        selected.push(utxo);
        if selected_value >= amount {
            break;
        }
    }
    if selected_value < amount {
        return Err(format!(
            "insufficient exported funds: requested {amount}, selected {selected_value}"
        )
        .into());
    }
    let mut outputs = vec![Note::new(amount, funding_public_key)];
    let change = selected_value - amount;
    if change > 0 {
        outputs.push(Note::new(change, funding_public_key));
    }
    let transfer = TransferOp {
        inputs: Inputs::try_new(selected.iter().map(Utxo::id).collect::<Vec<_>>())?,
        outputs: Outputs::try_new(outputs)?,
    };
    Ok((transfer, selected))
}

/// Build a channel deposit op that consumes the first transfer output.
pub fn build_deposit_op(
    channel_id: ChannelId,
    transfer: &TransferOp,
    metadata: &str,
) -> RunResult<DepositOp> {
    let deposit_note_id = transfer
        .outputs
        .utxo_by_index(0, transfer)
        .expect("deposit transfer always has an output")
        .id();
    Ok(DepositOp {
        channel_id,
        inputs: Inputs::new([deposit_note_id]),
        metadata: Metadata::try_from(metadata.as_bytes().to_vec())?,
    })
}

/// Start a zone sequencer for non-interactive CLI commands and wait for
/// readiness.
pub async fn start_cli_sequencer(args: &NodeKeyArgs) -> RunResult<ZoneSequencer<NodeHttpClient>> {
    let (sequencer, _channel_state) = start_cli_sequencer_with_channel_state(args).await?;
    Ok(sequencer)
}

/// Start a zone sequencer for non-interactive CLI commands and wait until the
/// post-ready channel view is backed by freshly queried node channel state.
pub async fn start_cli_sequencer_with_channel_state(
    args: &NodeKeyArgs,
) -> RunResult<(ZoneSequencer<NodeHttpClient>, Option<ChannelState>)> {
    let signing_key = load_or_create_signing_key(PathBuf::from(&args.key_path).as_path());
    let channel_id = resolve_channel_id(args)?;
    let node = node_client(&args.node_url)?;
    let channel_exists = query_channel_exists(&node, channel_id).await;
    let checkpoint = load_cli_checkpoint(&channel_id, channel_exists)?;
    let mut sequencer = ZoneSequencer::init(channel_id, signing_key, node.clone(), checkpoint);
    while !sequencer.is_ready() {
        drop(sequencer.next_event().await);
    }
    let channel_state = wait_for_fresh_channel_state(&mut sequencer, &node, channel_id).await?;
    Ok((sequencer, channel_state))
}

async fn wait_for_fresh_channel_state(
    sequencer: &mut ZoneSequencer<NodeHttpClient>,
    node: &NodeHttpClient,
    channel_id: ChannelId,
) -> RunResult<Option<ChannelState>> {
    let fresh_channel_state = query_channel_state(node, channel_id).await;
    let view_rx = sequencer.subscribe_channel_view();
    loop {
        if view_rx.borrow().channel == fresh_channel_state {
            return Ok(fresh_channel_state);
        }
        if let Event::BlocksProcessed { checkpoint, .. } = sequencer.next_event().await {
            save_cli_checkpoint(&channel_id, &checkpoint)?;
        }
    }
}

/// Load the persisted non-interactive sequencer checkpoint, if present.
pub fn load_cli_checkpoint(
    channel_id: &ChannelId,
    channel_exists: bool,
) -> RunResult<Option<SequencerCheckpoint>> {
    load_or_discard_persisted_checkpoint_for_channel(channel_id, channel_exists)
}

/// Persist a non-interactive sequencer checkpoint in the runtime directory.
pub fn save_cli_checkpoint(
    channel_id: &ChannelId,
    checkpoint: &SequencerCheckpoint,
) -> RunResult<()> {
    save_persisted_checkpoint(channel_id, checkpoint)
}
