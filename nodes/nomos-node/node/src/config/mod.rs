use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs as _},
    path::{Path, PathBuf},
};

use ::time::OffsetDateTime;
use chain_leader::LeaderConfig;
use clap::{Parser, ValueEnum, builder::OsStr};
use color_eyre::eyre::{Result, eyre};
use hex::FromHex as _;
use key_management_system_service::keys::UnsecuredZkKey;
use nomos_libp2p::{Multiaddr, ed25519::SecretKey};
use nomos_tracing::logging::{gelf::GelfConfig, local::FileConfig};
use nomos_tracing_service::{LoggerLayer, Tracing};
use num_bigint::BigUint;
use overwatch::services::ServiceData;
use serde::Deserialize;
use tracing::Level;

use crate::{
    ApiService, CryptarchiaService, DaNetworkService, DaSamplingService, DaVerifierService,
    KeyManagementService, RuntimeServiceId, StorageService,
    config::{
        blend::serde::Config as BlendConfig, cryptarchia::serde::Config as CryptarchiaConfig,
        deployment::DeploymentSettings, mempool::serde::Config as MempoolConfig,
        network::serde::Config as NetworkConfig, time::serde::Config as TimeConfig,
    },
    generic_services::{SdpService, WalletService},
};

pub mod blend;
pub mod cryptarchia;
pub mod deployment;
pub mod mempool;
pub mod network;
pub mod time;

#[cfg(test)]
mod tests;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct CliArgs {
    /// Path for a yaml-encoded network config file
    config: PathBuf,
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
    http: HttpArgs,
    #[clap(flatten)]
    cryptarchia_leader: CryptarchiaLeaderArgs,
    #[clap(flatten)]
    da: DaArgs,
    #[clap(flatten)]
    time: TimeArgs,
}

impl CliArgs {
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config
    }

    /// If flags the blend service group to start if either all service groups
    /// are flagged to start or the blend service group is.
    #[must_use]
    pub const fn dry_run(&self) -> bool {
        self.check_config_only
    }

    #[must_use]
    pub const fn must_blend_service_group_start(&self) -> bool {
        self.must_all_service_groups_start() || self.blend.start_blend_at_boot
    }

    /// If flags the DA service group to start if either all service groups are
    /// flagged to start or the DA service group is.
    #[must_use]
    pub const fn must_da_service_group_start(&self) -> bool {
        self.must_all_service_groups_start() || self.da.start_da_at_boot
    }

    /// If no "start" flag is explicitly set for any service group, then all
    /// service groups are flagged to start.
    const fn must_all_service_groups_start(&self) -> bool {
        !self.blend.start_blend_at_boot && !self.da.start_da_at_boot
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
    #[clap(long = "blend-service-group", action)]
    start_blend_at_boot: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct HttpArgs {
    #[clap(long = "http-host", env = "HTTP_HOST")]
    pub http_addr: Option<SocketAddr>,

    #[clap(long = "http-cors-origin", env = "HTTP_CORS_ORIGIN")]
    pub cors_origins: Option<Vec<String>>,
}

#[derive(Parser, Debug, Clone)]
pub struct CryptarchiaLeaderArgs {
    #[clap(long = "consensus-utxo-sk", env = "CONSENSUS_UTXO_SK")]
    pub secret_key: Option<String>,
}

#[derive(Parser, Debug, Clone)]
pub struct TimeArgs {
    #[clap(
        long = "consensus-chain-start",
        env = "CONSENSUS_CHAIN_START",
        group = "start_time"
    )]
    chain_start_time: Option<i64>,
    #[clap(long = "dev-mode-reset-chain-clock", group = "start_time")]
    dev_mode_reset_chain_clock: bool,
}

pub enum ChainStartMode {
    FromEnv(i64),
    FromConfig,
    Now,
}

impl TimeArgs {
    #[must_use]
    pub const fn to_mode(&self) -> ChainStartMode {
        if self.dev_mode_reset_chain_clock {
            ChainStartMode::Now
        } else if let Some(ts) = self.chain_start_time {
            ChainStartMode::FromEnv(ts)
        } else {
            ChainStartMode::FromConfig
        }
    }
}

#[derive(Parser, Debug, Clone)]
pub struct DaArgs {
    #[clap(long = "da-service-group", action)]
    start_da_at_boot: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "testing", derive(serde::Serialize))]
pub struct Config {
    pub network: NetworkConfig,
    pub blend: BlendConfig,
    pub deployment: DeploymentSettings,
    pub cryptarchia: CryptarchiaConfig,
    pub time: TimeConfig,
    pub mempool: MempoolConfig,

    pub tracing: <Tracing<RuntimeServiceId> as ServiceData>::Settings,
    pub da_network: <DaNetworkService as ServiceData>::Settings,
    pub da_verifier: <DaVerifierService as ServiceData>::Settings,
    pub sdp: <SdpService<RuntimeServiceId> as ServiceData>::Settings,
    pub da_sampling: <DaSamplingService as ServiceData>::Settings,
    pub http: <ApiService as ServiceData>::Settings,
    pub storage: <StorageService as ServiceData>::Settings,
    pub key_management: <KeyManagementService as ServiceData>::Settings,
    pub wallet: <WalletService<CryptarchiaService, RuntimeServiceId> as ServiceData>::Settings,

    #[cfg(feature = "testing")]
    pub testing_http: <ApiService as ServiceData>::Settings,
}

impl Config {
    pub fn update_from_args(mut self, args: CliArgs) -> Result<Self> {
        let CliArgs {
            log: log_args,
            http: http_args,
            network: network_args,
            blend: blend_args,
            cryptarchia_leader: cryptarchia_leader_args,
            time: time_args,
            ..
        } = args;
        update_tracing(&mut self.tracing, log_args)?;
        update_network(&mut self.network, network_args)?;
        update_blend(&mut self.blend, blend_args)?;
        update_http(&mut self.http, http_args)?;
        update_cryptarchia_leader_consensus(&mut self.cryptarchia.leader, cryptarchia_leader_args)?;
        update_time(&mut self.time, &time_args)?;
        Ok(self)
    }
}

pub fn update_tracing(
    tracing: &mut <Tracing<RuntimeServiceId> as ServiceData>::Settings,
    tracing_args: LogArgs,
) -> Result<()> {
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
            LoggerLayerType::Gelf => LoggerLayer::Gelf(GelfConfig {
                addr: addr
                    .ok_or_else(|| eyre!("Gelf backend requires an address."))?
                    .to_socket_addrs()?
                    .next()
                    .ok_or_else(|| eyre!("Invalid gelf address"))?,
            }),
            LoggerLayerType::File => LoggerLayer::File(FileConfig {
                directory: directory.ok_or_else(|| eyre!("File backend requires a directory."))?,
                prefix,
            }),
            LoggerLayerType::Stdout => LoggerLayer::Stdout,
            LoggerLayerType::Stderr => LoggerLayer::Stderr,
        }
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
    let BlendArgs { blend_addr, .. } = blend_args;

    if let Some(addr) = blend_addr {
        blend.set_listening_address(addr);
    }

    Ok(())
}

pub fn update_http(
    http: &mut <ApiService as ServiceData>::Settings,
    http_args: HttpArgs,
) -> Result<()> {
    let HttpArgs {
        http_addr,
        cors_origins,
    } = http_args;

    if let Some(addr) = http_addr {
        http.backend_settings.address = addr;
    }

    if let Some(cors) = cors_origins {
        http.backend_settings.cors_origins = cors;
    }

    Ok(())
}

pub fn update_cryptarchia_leader_consensus(
    leader: &mut LeaderConfig,
    consensus_args: CryptarchiaLeaderArgs,
) -> Result<()> {
    let CryptarchiaLeaderArgs { secret_key } = consensus_args;
    let Some(secret_key) = secret_key else {
        return Ok(());
    };

    let sk = UnsecuredZkKey::from(BigUint::from_bytes_le(&<[u8; 16]>::from_hex(secret_key)?));
    let pk = sk.to_public_key();

    leader.sk = sk;
    leader.pk = pk;

    Ok(())
}

pub fn update_time(time: &mut TimeConfig, time_args: &TimeArgs) -> Result<()> {
    match time_args.to_mode() {
        ChainStartMode::Now => {
            time.chain_start_time = OffsetDateTime::now_utc();
        }
        ChainStartMode::FromEnv(ts) => {
            time.chain_start_time = OffsetDateTime::from_unix_timestamp(ts)?;
        }
        ChainStartMode::FromConfig => {}
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

pub fn deserialize_config_at_path<Config>(
    config_path: &Path,
) -> Result<Config, ConfigDeserializationError<Config>>
where
    Config: for<'de> Deserialize<'de>,
{
    let mut ignored_fields = Vec::new();
    let config = serde_ignored::deserialize::<_, _, Config>(
        serde_yaml::Deserializer::from_reader(std::fs::File::open(config_path)?),
        |path| {
            ignored_fields.push(path.to_string());
        },
    )?;

    if ignored_fields.is_empty() {
        Ok(config)
    } else {
        Err(ConfigDeserializationError::UnrecognizedFields {
            fields: ignored_fields,
            config,
        })
    }
}
