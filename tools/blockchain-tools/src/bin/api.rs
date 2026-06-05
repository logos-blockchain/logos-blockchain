use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient};
use lb_core::{mantle::NoteId, sdp::Locator};
use lb_http_api_common::bodies::{
    blend::JoinBlendRequestBody, wallet::balance::WalletBalanceResponseBody,
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::config::UserConfig;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
use serde::{Deserialize, de::IntoDeserializer as _};
use url::Url;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.run().await
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Logos blockchain HTTP API utility",
    long_about = "Utilities for interacting with node HTTP APIs from the command line."
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

impl Cli {
    async fn run(self) -> Result<()> {
        self.command.run().await
    }
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Service Declaration Protocol (SDP) operations.
    Sdp {
        #[command(subcommand)]
        command: SdpSubCommand,
    },
}

impl CliCommand {
    async fn run(self) -> Result<()> {
        match self {
            Self::Sdp { command } => command.run().await,
        }
    }
}

#[derive(Debug, Subcommand)]
enum SdpSubCommand {
    /// Post a Blend SDP declaration using the specified locator address and ID
    /// of a note to lock.
    ///
    ///
    /// The command validates that:
    /// - the SDP funding key has non-zero balance
    /// - `--locked-note-id` exists for the Blend ZK key before submitting the
    ///   declaration.
    PostBlendDeclaration(PostBlendDeclarationArgs),
}

impl SdpSubCommand {
    async fn run(self) -> Result<()> {
        match self {
            Self::PostBlendDeclaration(args) => post_blend_declaration(args).await,
        }
    }
}

#[derive(Debug, Parser)]
struct PostBlendDeclarationArgs {
    /// Path to the node user config YAML file.
    #[arg(long, value_name = "USER_CONFIG_YAML")]
    user_config_path: PathBuf,

    /// Address of the Blend service to include in the declaration.
    ///
    /// This must be externally reachable, because listening addresses such as
    /// `0.0.0.0` are not valid `Locator` values for declarations.
    #[arg(long, value_name = "BLEND_ADDR")]
    blend_addr: Locator,

    /// Note ID to lock for the Blend declaration (HEX-encoded field element).
    #[arg(long, value_name = "NOTE_ID_HEX", value_parser = parse_hex_serde::<NoteId>)]
    locked_note_id: NoteId,

    /// Base node URL, for example `http://localhost:8080`.
    #[arg(long, value_name = "NODE_URL", default_value = "http://localhost:8080")]
    node_address: Url,

    /// Optional basic auth username for the API.
    #[arg(long, value_name = "USERNAME")]
    username: Option<String>,

    /// Optional basic auth password for the API.
    #[arg(long, value_name = "PASSWORD")]
    password: Option<String>,
}

fn parse_hex_serde<T>(input: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    use serde::de::value::Error;

    T::deserialize(input.into_deserializer())
        .map_err(|e: Error| format!("Failed to parse input HEX string: {e}"))
}

async fn post_blend_declaration(
    PostBlendDeclarationArgs {
        locked_note_id,
        blend_addr,
        node_address,
        user_config_path,
        username,
        password,
    }: PostBlendDeclarationArgs,
) -> Result<()> {
    let user_config =
        deserialize_value_at_path::<UserConfig>(&user_config_path, OnUnknownKeys::Fail)
            .with_context(|| {
                format!(
                    "Failed to read user config at '{}'",
                    user_config_path.display()
                )
            })?;

    let client = {
        let credentials = username.map(|u| BasicAuthCredentials::new(u, password));
        CommonHttpClient::new(credentials)
    };

    validate_config_values(&client, node_address.clone(), &user_config, locked_note_id)
        .await
        .with_context(|| "Failed to validate values from user config")?;

    let declaration_id = client
        .join_blend_network(
            &node_address,
            JoinBlendRequestBody {
                locator: blend_addr,
                locked_note_id,
            },
        )
        .await
        .context("Failed to post Blend join network declaration")?;

    println!("Declaration posted successfully: {declaration_id}");
    Ok(())
}

async fn validate_config_values(
    client: &CommonHttpClient,
    node_address: Url,
    config: &UserConfig,
    locked_note_id: NoteId,
) -> Result<()> {
    let sdp_wallet_funding_pk = config.sdp.wallet.funding_pk;
    verify_sdp_wallet_funding_pk_balance(client, node_address.clone(), sdp_wallet_funding_pk)
        .await
        .context("Failed to verify balance for SDP wallet funding key")?;

    let zk_id = extract_blend_zk_key(config)?;

    verify_locked_note_id_value(client, node_address, zk_id, locked_note_id).await?;

    Ok(())
}

fn extract_blend_zk_key(config: &UserConfig) -> Result<ZkPublicKey> {
    let (zk_public_key_id, zk_public_key) = config
        .blend_zk_key()
        .map_err(|e| anyhow!(e))
        .with_context(|| "Failed to extract zk ID from provided config.")?;
    let Some(wallet_key) = config.wallet.known_keys.get(&zk_public_key_id) else {
        bail!(
            "ZK ID '{zk_public_key_id}' extracted from config was not found in wallet known keys"
        );
    };
    if wallet_key != &zk_public_key {
        bail!(
            "ZK ID '{zk_public_key_id}' extracted from config does not match the corresponding public key in wallet known keys"
        );
    }
    Ok(zk_public_key)
}

async fn verify_sdp_wallet_funding_pk_balance(
    client: &CommonHttpClient,
    node_address: Url,
    funding_pk: ZkPublicKey,
) -> Result<()> {
    let WalletBalanceResponseBody { balance, .. } = client
        .get_wallet_balance(node_address, funding_pk, None)
        .await
        .context("Failed to fetch wallet balance for SDP wallet funding pk.")?;

    // TODO: Strengthen this preflight check to verify fee sufficiency, not only
    // non-zero balance.
    if balance == 0 {
        bail!("The provided SDP wallet funding key does not have any balance");
    }

    Ok(())
}

async fn verify_locked_note_id_value(
    client: &CommonHttpClient,
    node_address: Url,
    zk_id: ZkPublicKey,
    locked_note_id: NoteId,
) -> Result<()> {
    let WalletBalanceResponseBody { notes, .. } = client
        .get_wallet_balance(node_address, zk_id, None)
        .await
        .context("Failed to fetch wallet balance for Blend ZK ID")?;

    // Preflight guard: fail early when the provided note does not belong to the
    // declaration ZK key according to the wallet view at `node_address`.
    // TODO: Also verify minimum stake amount once that threshold is exposed here.
    if !notes.contains_key(&locked_note_id) {
        bail!(
            "Locked note ID '{locked_note_id:?}' was not found in wallet notes for provided Blend ZK ID",
        );
    }
    Ok(())
}
