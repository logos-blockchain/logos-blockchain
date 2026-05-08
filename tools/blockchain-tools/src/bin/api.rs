use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient};
use lb_core::{
    mantle::NoteId,
    sdp::{DeclarationMessage, Locator, ProviderId, ServiceType},
};
use lb_http_api_common::bodies::wallet::balance::WalletBalanceResponseBody;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::config::{OnUnknownKeys, UserConfig, deserialize_config_at_path};
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
    /// Post a Blend SDP declaration using values extracted from the user
    /// config.
    ///
    /// The command derives the following from `--user-config-path`:
    /// - `provider_id` (from Blend non-ephemeral signing key)
    /// - `zk_id` (from Blend core ZK key)
    /// - `locator` (from Blend core listening address, unless overridden by
    ///   `--blend-addr`)
    ///
    /// It then validates that `--locked-note-id` exists for that ZK key before
    /// submitting the declaration.
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

    /// Address of the Blend service to use in the declaration that overrides
    /// the one present in the config file. This is useful for the case in which
    /// a node is listening on the `0.0.0.0` address, and the declaration needs
    /// to be posted with the externally reachable address, since `0.0.0.0` is
    /// not a valid `Locator` value.
    #[arg(long, value_name = "BLEND_ADDR")]
    blend_addr: Option<Locator>,

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
        deserialize_config_at_path::<UserConfig>(&user_config_path, OnUnknownKeys::Warn)
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

    let ExtractedUserConfigValues {
        provider_id,
        zk_id,
        locked_note_id,
        locator,
    } = extract_values(
        &client,
        node_address.clone(),
        &user_config,
        locked_note_id,
        blend_addr,
    )
    .await
    .with_context(|| "Failed to extract necessary values from user config")?;

    let declaration = DeclarationMessage {
        locators: vec![locator],
        locked_note_id,
        provider_id,
        service_type: ServiceType::BlendNetwork,
        zk_id,
    };

    let declaration_id = client
        .post_declaration(node_address, &declaration)
        .await
        .context("Failed to post declaration")?;

    println!("Declaration posted successfully: {declaration_id}");
    Ok(())
}

struct ExtractedUserConfigValues {
    provider_id: ProviderId,
    zk_id: ZkPublicKey,
    locked_note_id: NoteId,
    locator: Locator,
}

async fn extract_values(
    client: &CommonHttpClient,
    node_address: Url,
    config: &UserConfig,
    locked_note_id: NoteId,
    blend_address: Option<Locator>,
) -> Result<ExtractedUserConfigValues> {
    // Keep all config-derived declaration fields in one place so the CLI and
    // node service remain aligned on identity/key source semantics.
    let locator = if let Some(blend_address) = blend_address {
        blend_address
    } else {
        extract_blend_locator(config)?
    };

    let provider_id = config
        .blend_provider_id()
        .map_err(|e| anyhow!(e))
        .with_context(|| "Failed to extract provider ID from provided config.")?;

    let zk_id = extract_blend_zk_key(config)?;

    verify_locked_note_id_value(client, node_address, zk_id, locked_note_id).await?;

    Ok(ExtractedUserConfigValues {
        provider_id,
        zk_id,
        locked_note_id,
        locator,
    })
}

// Validate and return the Blend listening address from the provided config.
fn extract_blend_locator(config: &UserConfig) -> Result<Locator> {
    config
        .blend
        .core
        .backend
        .listening_address
        .clone()
        .try_into()
        .map_err(|_| {
            anyhow!(
                "Blend listening address '{:?}' from config is not a valid locator",
                config.blend.core.backend.listening_address
            )
        })
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
