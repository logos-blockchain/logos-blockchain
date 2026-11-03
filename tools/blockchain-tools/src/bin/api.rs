use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient};
use lb_core::{mantle::Value, sdp::Locator};
use lb_http_api_common::bodies::{
    blend::JoinBlendRequestBody, wallet::balance::WalletBalanceResponseBody,
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::config::UserConfig;
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
    /// Post a Blend SDP declaration using the specified locator address and
    /// stake amount, which the node's wallet funds.
    ///
    ///
    /// The command validates that:
    /// - the SDP funding key has non-zero balance
    /// - the Blend ZK key is a wallet known key before submitting the
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

    /// Stake of the Blend declaration. When omitted, the minimum stake.
    #[arg(long, value_name = "AMOUNT")]
    amount: Option<Value>,

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

async fn post_blend_declaration(
    PostBlendDeclarationArgs {
        amount,
        blend_addr,
        node_address,
        user_config_path,
        username,
        password,
    }: PostBlendDeclarationArgs,
) -> Result<()> {
    let user_config = UserConfig::load(&user_config_path).map_err(|error| {
        anyhow!(
            "Failed to read user config at '{}': {error}",
            user_config_path.display()
        )
    })?;

    let client = {
        let credentials = username.map(|u| BasicAuthCredentials::new(u, password));
        CommonHttpClient::new(credentials)
    };

    validate_config_values(&client, node_address.clone(), &user_config)
        .await
        .with_context(|| "Failed to validate values from user config")?;

    let submission = client
        .join_blend_network(
            &node_address,
            JoinBlendRequestBody {
                locator: blend_addr,
                amount,
            },
        )
        .await
        .context("Failed to post Blend join network declaration")?;

    println!(
        "Declaration posted successfully: {} (tx id: {:?})",
        submission.declaration_id, submission.tx_id
    );
    Ok(())
}

async fn validate_config_values(
    client: &CommonHttpClient,
    node_address: Url,
    config: &UserConfig,
) -> Result<()> {
    let sdp_wallet_funding_pk = config.sdp.wallet.funding_pk;
    verify_sdp_wallet_funding_pk_balance(client, node_address, sdp_wallet_funding_pk)
        .await
        .context("Failed to verify balance for SDP wallet funding key")?;

    extract_blend_zk_key(config)?;

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
