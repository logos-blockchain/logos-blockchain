use core::{convert::Infallible, str::FromStr};
use std::{
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs as _},
    path::{Path, PathBuf},
};

use ::tracing::{Level, warn};
use clap::{Parser, Subcommand, ValueEnum, builder::OsStr};
use color_eyre::eyre::{Result, eyre};
use lb_libp2p::{Multiaddr, ed25519::SecretKey};
use serde::Deserialize;

use crate::config::tracing::serde::logger::{FileConfig, GelfConfig, Layer};
pub use crate::config::{
    api::serde::Config as ApiConfig,
    blend::serde::Config as BlendConfig,
    cryptarchia::serde::Config as CryptarchiaConfig,
    deployment::{DeploymentSettings, WellKnownDeployment},
    kms::serde::Config as KmsConfig,
    mempool::serde::Config as MempoolConfig,
    network::serde::Config as NetworkConfig,
    sdp::serde::Config as SdpConfig,
    storage::serde::Config as StorageConfig,
    time::serde::Config as TimeConfig,
    tracing::serde::Config as TracingConfig,
    wallet::serde::Config as WalletConfig,
};

pub mod api;
pub mod blend;
pub mod cryptarchia;
pub mod deployment;
pub mod kms;
pub mod mempool;
pub mod network;
pub mod sdp;
pub mod storage;
pub mod time;
pub mod tracing;
pub mod wallet;

#[cfg(test)]
mod tests;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None,
          args_conflicts_with_subcommands = true,
          subcommand_negates_reqs = true)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path for a yaml-encoded network config file
    config: Option<PathBuf>,
    /// Dry-run flag. If active, the binary will try to deserialize the config
    /// file and then exit.
    #[clap(long = "check-config", action)]
    check_config_only: bool,
    /// Overrides log config.
    #[clap(flatten)]
    log: LogArgs,
    /// Overrides network config.
    #[clap(flatten)]
    network: NetworkArgs,
    /// Overrides blend config.
    #[clap(flatten)]
    blend: BlendArgs,
    /// Overrides http config.
    #[clap(flatten)]
    api: ApiArgs,
    #[clap(flatten)]
    deployment: DeploymentArgs,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a new user config with generated keys
    #[cfg(feature = "config-gen")]
    Init(InitArgs),
    /// Publish text inscriptions as zone blocks
    Inscribe(logos_blockchain_tui_zone::InscribeArgs),
}

#[cfg(feature = "config-gen")]
#[derive(Parser, Debug)]
pub struct InitArgs {
    /// Trusted peers to bootstrap from (multiaddr format)
    #[clap(long = "initial-peers", short = 'p', num_args = 1.., value_delimiter = ',')]
    pub initial_peers: Vec<Multiaddr>,

    /// Output file path for the generated config
    #[clap(long = "output", short = 'o', default_value = "user_config.yaml")]
    pub output: PathBuf,

    /// Network listen port
    #[clap(long = "net-port", default_value = "3000")]
    pub net_port: u16,

    /// Blend listen port
    #[clap(long = "blend-port", default_value = "3400")]
    pub blend_port: u16,

    /// HTTP API listen address
    #[clap(long = "http-addr", default_value = "0.0.0.0:8080")]
    pub http_addr: SocketAddr,

    /// External address for nodes with a known public IP (disables NAT
    /// traversal). Format: /ip4/<public-ip>/udp/<port>/quic-v1
    #[clap(long = "external-address")]
    pub external_address: Option<Multiaddr>,

    /// Skip automatic public IP detection
    #[clap(long = "no-public-ip-check")]
    pub no_public_ip_check: bool,

    /// Deployment configuration (well-known name or path to custom config)
    #[clap(long = "deployment", default_value = DeploymentType::default())]
    pub deployment: DeploymentType,
}

impl CliArgs {
    #[must_use]
    pub fn config_path(&self) -> &Path {
        self.config
            .as_deref()
            .expect("config path is required when not using a subcommand")
    }

    #[must_use]
    pub const fn dry_run(&self) -> bool {
        self.check_config_only
    }

    #[must_use]
    pub const fn deployment_type(&self) -> &DeploymentType {
        &self.deployment.deployment_type
    }
}

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum LoggerLayerType {
    Gelf,
    File,
    #[default]
    Stdout,
    Stderr,
}

impl From<LoggerLayerType> for OsStr {
    fn from(value: LoggerLayerType) -> Self {
        match value {
            LoggerLayerType::Gelf => "Gelf".into(),
            LoggerLayerType::File => "File".into(),
            LoggerLayerType::Stderr => "Stderr".into(),
            LoggerLayerType::Stdout => "Stdout".into(),
        }
    }
}

#[derive(Parser, Debug, Clone)]
pub struct LogArgs {
    /// Address for the Gelf backend
    #[clap(
        long = "log-addr",
        env = "LOG_ADDR",
        required_if_eq("backend", LoggerLayerType::Gelf)
    )]
    log_addr: Option<String>,

    /// Directory for the File backend
    #[clap(
        long = "log-dir",
        env = "LOG_DIR",
        required_if_eq("backend", LoggerLayerType::File)
    )]
    directory: Option<PathBuf>,

    /// Prefix for the File backend
    #[clap(
        long = "log-path",
        env = "LOG_PATH",
        required_if_eq("backend", LoggerLayerType::File)
    )]
    prefix: Option<PathBuf>,

    /// Backend type
    #[clap(long = "log-backend", env = "LOG_BACKEND", value_enum)]
    backend: Option<LoggerLayerType>,

    #[clap(long = "log-level", env = "LOG_LEVEL")]
    level: Option<String>,
}

#[derive(Parser, Debug, Clone)]
pub struct NetworkArgs {
    #[clap(long = "net-host", env = "NET_HOST")]
    host: Option<IpAddr>,

    #[clap(long = "net-port", env = "NET_PORT")]
    port: Option<usize>,

    // TODO: Use either the raw bytes or the key type directly to delegate error handling to clap
    #[clap(long = "net-node-key", env = "NET_NODE_KEY")]
    node_key: Option<String>,

    #[clap(long = "net-initial-peers", env = "NET_INITIAL_PEERS", num_args = 1.., value_delimiter = ',')]
    pub initial_peers: Option<Vec<Multiaddr>>,
}

#[derive(Parser, Debug, Clone)]
pub struct BlendArgs {
    #[clap(long = "blend-addr", env = "BLEND_ADDR")]
    blend_addr: Option<Multiaddr>,
}

#[derive(Parser, Debug, Clone)]
pub struct ApiArgs {
    #[clap(long = "http-host", env = "HTTP_HOST")]
    pub addr: Option<SocketAddr>,

    #[clap(long = "http-cors-origin", env = "HTTP_CORS_ORIGIN")]
    pub cors_origins: Option<Vec<String>>,
}

#[derive(Parser, Debug, Clone)]
pub struct DeploymentArgs {
    #[clap(long = "deployment", env = "DEPLOYMENT", default_value = DeploymentType::default())]
    deployment_type: DeploymentType,
}

impl DeploymentArgs {
    #[must_use]
    pub const fn deployment_type(&self) -> &DeploymentType {
        &self.deployment_type
    }
}

#[derive(Debug, Clone)]
pub enum DeploymentType {
    WellKnown(WellKnownDeployment),
    Custom(PathBuf),
}

impl Default for DeploymentType {
    fn default() -> Self {
        WellKnownDeployment::default().into()
    }
}

impl From<WellKnownDeployment> for DeploymentType {
    fn from(deployment: WellKnownDeployment) -> Self {
        Self::WellKnown(deployment)
    }
}

impl From<PathBuf> for DeploymentType {
    fn from(path: PathBuf) -> Self {
        Self::Custom(path)
    }
}

#[expect(clippy::fallible_impl_from, reason = "`From` impl required by clap.")]
impl From<DeploymentType> for OsStr {
    fn from(value: DeploymentType) -> Self {
        match value {
            DeploymentType::WellKnown(well_known_deployment) => {
                well_known_deployment.to_string().into()
            }
            DeploymentType::Custom(path) => path.to_str().unwrap().to_owned().into(),
        }
    }
}

impl FromStr for DeploymentType {
    type Err = Infallible;

    // Try to parse as a well-known deployment first, otherwise treat as a path.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.parse::<WellKnownDeployment>()
            .map_or_else(|()| PathBuf::from(s).into(), Into::into))
    }
}

#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(
    any(feature = "testing", feature = "config-gen"),
    derive(serde::Serialize)
)]
pub struct UserConfig {
    #[serde(default)]
    pub network: NetworkConfig,
    pub blend: BlendConfig,
    pub cryptarchia: CryptarchiaConfig,
    #[serde(default)]
    pub time: TimeConfig,
    #[serde(default)]
    pub mempool: MempoolConfig,
    pub sdp: SdpConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub kms: KmsConfig,
    pub wallet: WalletConfig,
    #[serde(default)]
    pub tracing: TracingConfig,
}

impl UserConfig {
    pub fn update_from_args(mut self, args: CliArgs) -> Result<RunConfig> {
        let CliArgs {
            log: log_args,
            api: api_args,
            network: network_args,
            blend: blend_args,
            deployment: deployment_args,
            ..
        } = args;
        update_tracing(&mut self.tracing, log_args)?;
        update_network(&mut self.network, network_args)?;
        update_blend(&mut self.blend, blend_args)?;
        update_api(&mut self.api, api_args)?;

        let deployment_settings = match deployment_args.deployment_type() {
            DeploymentType::WellKnown(well_known_deployment) => (*well_known_deployment).into(),
            DeploymentType::Custom(custom_deployment_config_path) => {
                deserialize_config_at_path::<DeploymentSettings>(
                    custom_deployment_config_path,
                    OnUnknownKeys::Warn,
                )?
            }
        };

        Ok(RunConfig {
            deployment: deployment_settings,
            user: self,
        })
    }
}

pub fn update_tracing(tracing: &mut TracingConfig, tracing_args: LogArgs) -> Result<()> {
    let LogArgs {
        backend,
        log_addr: addr,
        directory,
        prefix,
        level,
    } = tracing_args;

    // Override the file config with the one from env variables.
    if let Some(backend) = backend {
        tracing.logger = match backend {
            LoggerLayerType::Gelf => Layer::Gelf(GelfConfig {
                addr: addr
                    .ok_or_else(|| eyre!("Gelf backend requires an address."))?
                    .to_socket_addrs()?
                    .next()
                    .ok_or_else(|| eyre!("Invalid gelf address"))?,
            }),
            LoggerLayerType::File => Layer::File(FileConfig {
                directory: directory.ok_or_else(|| eyre!("File backend requires a directory."))?,
                prefix,
            }),
            LoggerLayerType::Stdout => Layer::Stdout,
            LoggerLayerType::Stderr => Layer::Stderr,
        };
    }

    if let Some(level_str) = level {
        tracing.level = match level_str.as_str() {
            "DEBUG" => Level::DEBUG,
            "INFO" => Level::INFO,
            "ERROR" => Level::ERROR,
            "WARN" => Level::WARN,
            _ => return Err(eyre!("Invalid log level provided.")),
        };
    }
    Ok(())
}

pub fn update_network(network: &mut NetworkConfig, network_args: NetworkArgs) -> Result<()> {
    let NetworkArgs {
        host,
        port,
        node_key,
        initial_peers,
    } = network_args;

    if let Some(IpAddr::V4(h)) = host {
        network.backend.swarm.host = h;
    } else if host.is_some() {
        return Err(eyre!("Unsupported ip version"));
    }

    if let Some(port) = port {
        network.backend.swarm.port = port as u16;
    }

    if let Some(node_key) = node_key {
        let mut key_bytes = hex::decode(node_key)?;
        network.backend.swarm.node_key = SecretKey::try_from_bytes(key_bytes.as_mut_slice())?;
    }

    if let Some(peers) = initial_peers {
        network.backend.initial_peers = peers;
    }

    Ok(())
}

pub fn update_blend(blend: &mut BlendConfig, blend_args: BlendArgs) -> Result<()> {
    let BlendArgs { blend_addr } = blend_args;

    if let Some(addr) = blend_addr {
        blend.set_listening_address(addr);
    }

    Ok(())
}

pub fn update_api(api: &mut ApiConfig, args: ApiArgs) -> Result<()> {
    let ApiArgs { addr, cors_origins } = args;

    if let Some(addr) = addr {
        api.backend.listen_address = addr;
    }

    if let Some(cors) = cors_origins {
        api.backend.cors_origins = cors;
    }

    Ok(())
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigDeserializationError<Config> {
    #[error("Unrecognized fields in config: {fields:?}")]
    UnrecognizedFields { fields: Vec<String>, config: Config },
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    SerdeError(#[from] serde_yaml::Error),
}

pub enum OnUnknownKeys {
    Fail,
    Warn,
}

pub fn deserialize_config_at_path<Config>(
    config_path: &Path,
    unknown_keys_strategy: OnUnknownKeys,
) -> Result<Config, ConfigDeserializationError<Config>>
where
    Config: for<'de> Deserialize<'de>,
{
    let file = std::fs::File::open(config_path)?;
    deserialize_config_from_reader(file, unknown_keys_strategy)
}

pub fn deserialize_config_from_reader<Config, Reader>(
    reader: Reader,
    unknown_keys_strategy: OnUnknownKeys,
) -> Result<Config, ConfigDeserializationError<Config>>
where
    Config: for<'de> Deserialize<'de>,
    Reader: Read,
{
    let mut ignored_fields = Vec::new();
    let config = serde_ignored::deserialize::<_, _, Config>(
        serde_yaml::Deserializer::from_reader(reader),
        |path| {
            ignored_fields.push(path.to_string());
        },
    )?;

    match (ignored_fields, unknown_keys_strategy) {
        (ignored_fields, _) if ignored_fields.is_empty() => Ok(config),
        (ignored_fields, OnUnknownKeys::Warn) => {
            warn!(
                "The following unrecognized fields were found in the config: {ignored_fields:?}."
            );
            Ok(config)
        }
        (ignored_fields, OnUnknownKeys::Fail) => {
            Err(ConfigDeserializationError::UnrecognizedFields {
                fields: ignored_fields,
                config,
            })
        }
    }
}

/// Configuration for a running node. It is the combination of user-provided and
/// deployment-specific settings.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "testing", derive(serde::Serialize, serde::Deserialize))]
pub struct RunConfig {
    #[cfg_attr(feature = "testing", serde(flatten))]
    pub user: UserConfig,
    pub deployment: DeploymentSettings,
}

impl From<RunConfig> for UserConfig {
    fn from(value: RunConfig) -> Self {
        value.user
    }
}
