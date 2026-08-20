//! Startup configuration supplied through flags or environment variables.

use std::path::PathBuf;

use clap::Parser;
use lb_groth16::fr_from_bytes;
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_zone_sdk::{node_types::ChannelId, sequencer::FundingConfig};
use logos_sql::LogosSqlConfig;
use reqwest::Url;

#[derive(Parser)]
#[command(name = "password-manager")]
struct Options {
    /// Channel carrying the shared password-manager writes.
    #[arg(long, env = "LOGOS_SQL_CHANNEL_ID", value_parser = parse_channel_id)]
    channel_id: ChannelId,

    /// Ed25519 secret key used to sign inscriptions.
    #[arg(long, env = "LOGOS_SQL_SIGNING_KEY", value_parser = parse_signing_key)]
    signing_key: Ed25519Key,

    /// Public key funding inscription fees.
    #[arg(long, env = "LOGOS_SQL_FUNDING_KEY", value_parser = parse_funding_key)]
    funding_key: ZkPublicKey,

    /// Maximum fee accepted for one inscription.
    #[arg(long, env = "LOGOS_SQL_MAX_TX_FEE")]
    max_tx_fee: u64,

    /// Percentage of the maximum fee offered as priority fee.
    #[arg(
        long,
        env = "LOGOS_SQL_PRIORITY_FEE_PERCENT",
        default_value_t = FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT
    )]
    priority_fee_percent: u64,

    /// Node HTTP API used by `ZoneSDK`.
    #[arg(long, env = "LOGOS_SQL_NODE_URL")]
    node_url: Url,

    /// Participant-local directory containing the `SQLite` databases.
    #[arg(long, env = "LOGOS_SQL_STATE_DIR")]
    state_dir: PathBuf,
}

/// Parses startup options and builds the `λSQL` configuration.
pub fn from_args() -> LogosSqlConfig {
    let options = Options::parse();

    LogosSqlConfig {
        channel_id: options.channel_id,
        signing_key: options.signing_key,
        node_url: options.node_url,
        funding: FundingConfig {
            funding_pk: options.funding_key,
            max_tx_fee: options.max_tx_fee.into(),
            priority_fee_percent: options.priority_fee_percent,
        },
        state_dir: options.state_dir,
    }
}

fn parse_channel_id(value: &str) -> Result<ChannelId, String> {
    parse_hex(value).map(ChannelId::from)
}

fn parse_signing_key(value: &str) -> Result<Ed25519Key, String> {
    parse_hex(value).map(|bytes| Ed25519Key::from_bytes(&bytes))
}

fn parse_funding_key(value: &str) -> Result<ZkPublicKey, String> {
    let bytes = hex::decode(value).map_err(|error| error.to_string())?;

    fr_from_bytes(&bytes)
        .map(ZkPublicKey::new)
        .map_err(|error| error.to_string())
}

fn parse_hex<const N: usize>(value: &str) -> Result<[u8; N], String> {
    let bytes = hex::decode(value).map_err(|error| error.to_string())?;

    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("expected {N} bytes, received {}", bytes.len()))
}
