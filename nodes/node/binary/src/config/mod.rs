use core::{convert::Infallible, str::FromStr};
use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs as _},
    path::PathBuf,
    time::Duration,
};

use clap::{Parser, ValueEnum, builder::OsStr};
use color_eyre::eyre::{Result, eyre};
use lb_core::sdp::ProviderId;
use lb_groth16::fr_from_bytes;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Key, UnsecuredZkKey, ZkPublicKey},
};
use lb_libp2p::{Multiaddr, ed25519::SecretKey};
use lb_tracing::{
    filter::envfilter::{default_envfilter_config, parse_filter_directives},
    logging::local::{AppenderType, CompressionType, RetentionType, RollingConfig, RotationType},
};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

pub use crate::config::{
    api::serde::Config as ApiConfig,
    blend::serde::Config as BlendConfig,
    cryptarchia::serde::Config as CryptarchiaConfig,
    deployment::{DeploymentSettings, WellKnownDeployment},
    kms::serde::Config as KmsConfig,
    network::serde::Config as NetworkConfig,
    sdp::serde::Config as SdpConfig,
    state::Config as StateConfig,
    storage::serde::Config as StorageConfig,
    time::serde::Config as TimeConfig,
    tracing::serde::Config as TracingConfig,
    wallet::serde::Config as WalletConfig,
};
use crate::config::{
    network::serde::nat,
    tracing::serde::{
        filter::{EnvConfig, Layer},
        logger::{FileConfig, GelfConfig},
    },
};

pub mod api;
pub mod blend;
pub mod cryptarchia;
pub mod deployment;
pub mod kms;
pub mod mempool;
pub mod network;
pub mod sdp;
pub mod state;
pub mod storage;
pub mod time;
pub mod tracing;
pub mod wallet;

#[cfg(test)]
mod tests;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct UserConfig {
    #[serde(default)]
    pub network: NetworkConfig,
    pub blend: BlendConfig,
    pub cryptarchia: CryptarchiaConfig,
    #[serde(default)]
    pub time: TimeConfig,
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
    #[serde(default)]
    pub state: StateConfig,
}

pub struct RequiredValues {
    pub blend: BlendConfig,
    pub cryptarchia: CryptarchiaConfig,
    pub sdp: SdpConfig,
    pub wallet: WalletConfig,
}

impl UserConfig {
    #[must_use]
    pub fn with_required_values(required_values: RequiredValues) -> Self {
        Self {
            blend: required_values.blend,
            cryptarchia: required_values.cryptarchia,
            sdp: required_values.sdp,
            wallet: required_values.wallet,

            api: ApiConfig::default(),
            kms: KmsConfig::default(),
            network: NetworkConfig::default(),
            state: StateConfig::default(),
            storage: StorageConfig::default(),
            time: TimeConfig::default(),
            tracing: TracingConfig::default(),
        }
    }

    pub fn blend_provider_id(&self) -> Result<ProviderId, String> {
        let key_id = &self.blend.non_ephemeral_signing_key_id;
        let Some(key) = self.kms.backend.keys.get(key_id) else {
            return Err(format!(
                "Blend non-ephemeral signing key '{key_id}' not found in KMS"
            ));
        };
        let Key::Ed25519(secret_key) = key else {
            return Err("Blend non-ephemeral signing key must be Ed25519".to_owned());
        };
        Ok(ProviderId(secret_key.public_key()))
    }

    pub fn blend_zk_key(&self) -> Result<(String, ZkPublicKey), String> {
        let key_id = &self.blend.core.zk.secret_key_kms_id;
        let Some(key) = self.kms.backend.keys.get(key_id) else {
            return Err(format!("Blend ZK signing key '{key_id}' not found in KMS"));
        };
        let Key::Zk(secret_key) = key else {
            return Err("Blend ZK signing key must be Zk".to_owned());
        };
        Ok((key_id.to_owned(), secret_key.to_public_key()))
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

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum LogFileAppenderType {
    #[default]
    Simple,
    Rolling,
    RollingCompressed,
    RollingMaxFiles,
}

impl From<LogFileAppenderType> for OsStr {
    fn from(value: LogFileAppenderType) -> Self {
        match value {
            LogFileAppenderType::Simple => "Simple".into(),
            LogFileAppenderType::Rolling => "Rolling".into(),
            LogFileAppenderType::RollingCompressed => "RollingCompressed".into(),
            LogFileAppenderType::RollingMaxFiles => "RollingMaxFiles".into(),
        }
    }
}

#[derive(Parser, Debug, Default, Clone)]
pub struct LogArgs {
    /// Address for the Gelf backend
    #[clap(
        long = "log-addr",
        env = "LOG_ADDR",
        required_if_eq("backend", LoggerLayerType::Gelf)
    )]
    pub log_addr: Option<String>,

    /// Directory for the File backend
    #[clap(
        long = "log-dir",
        env = "LOG_DIR",
        required_if_eq("backend", LoggerLayerType::File)
    )]
    pub directory: Option<PathBuf>,

    /// Prefix for the File backend
    #[clap(
        long = "log-path",
        env = "LOG_PATH",
        required_if_eq("backend", LoggerLayerType::File)
    )]
    pub prefix: Option<PathBuf>,

    /// Backend type
    #[clap(long = "log-backend", env = "LOG_BACKEND", value_enum)]
    pub backend: Option<LoggerLayerType>,

    #[clap(long = "log-level", env = "LOG_LEVEL")]
    pub level: Option<String>,

    /// Per-target log filter directives, e.g.
    /// `libp2p_gossipsub=info,h2=warn`
    #[clap(long = "log-filter", env = "LOG_FILTER")]
    pub filter: Option<String>,

    #[clap(long = "log-file-appender", env = "LOG_APPENDER")]
    pub file_appender: Option<LogFileAppenderType>,

    #[clap(
        long = "log-max-files",
        env = "LOG_APPENDER_MAX_FILES",
        required_if_eq("file_appender", LogFileAppenderType::RollingMaxFiles)
    )]
    pub max_files: Option<usize>,
}

#[derive(Parser, Debug, Default, Clone)]
pub struct NetworkArgs {
    #[clap(long = "net-host", env = "NET_HOST")]
    pub host: Option<IpAddr>,

    #[clap(long = "net-port", env = "NET_PORT")]
    pub port: Option<u16>,

    // TODO: Use either the raw bytes or the key type directly to delegate error handling to clap
    #[clap(long = "net-node-key", env = "NET_NODE_KEY", value_parser = parse_hex_ed25519_key)]
    pub node_key: Option<SecretKey>,

    /// External address for nodes with a known public IP (disables NAT
    /// traversal). Format: /ip4/<public-ip>/udp/<port>/quic-v1
    #[clap(long = "external-address")]
    pub external_address: Option<Multiaddr>,

    #[clap(
        long = "net-initial-peers",
        short = 'p',
        env = "NET_INITIAL_PEERS",
        num_args = 1..,
        value_delimiter = ','
    )]
    pub initial_peers: Option<Vec<Multiaddr>>,
}

#[derive(Parser, Debug, Default, Clone)]
pub struct BlendArgs {
    #[clap(long = "blend-addr", env = "BLEND_ADDR")]
    pub blend_addr: Option<Multiaddr>,

    #[clap(long = "blend-signing-key-id", env = "BLEND_SIGNING_KEY_ID")]
    pub blend_signing_key_id: Option<KeyId>,

    #[clap(long = "blend-secret-key-id", env = "BLEND_SECRET_KEY_ID")]
    pub blend_secret_key_id: Option<KeyId>,
}

#[derive(Parser, Debug, Default, Clone, Copy)]
pub struct CryptarchiaArgs {
    #[clap(
        long = "cryptarchia-funding-pk",
        env = "CRYPTARCHIA_FUNDING_PK",
        value_parser = parse_hex_public_key
    )]
    pub cryptarchia_funding_pk: Option<ZkPublicKey>,

    /// Enable Initial Block Download (IBD) using peers
    /// passed via `--net-initial-peers`/`-p`.
    #[clap(long = "ibd", default_value_t = false)]
    pub ibd: bool,
}

#[derive(Parser, Debug, Default, Clone, Copy)]
pub struct SdpArgs {
    #[clap(
        long = "sdp-funding-pk",
        env = "SDP_FUNDING_PK",
        value_parser = parse_hex_public_key
    )]
    pub sdp_funding_pk: Option<ZkPublicKey>,
}

#[derive(Parser, Debug, Default, Clone)]
pub struct ApiArgs {
    #[clap(long = "http-host", env = "HTTP_HOST")]
    pub addr: Option<SocketAddr>,

    #[clap(long = "http-cors-origin", env = "HTTP_CORS_ORIGIN")]
    pub cors_origins: Option<Vec<String>>,
}

#[derive(Parser, Debug, Default, Clone)]
pub struct StateArgs {
    #[clap(long = "state-path", env = "STATE_PATH")]
    pub path: Option<PathBuf>,
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

pub fn update_tracing(tracing: &mut TracingConfig, tracing_args: LogArgs) -> Result<()> {
    let LogArgs {
        backend,
        log_addr: addr,
        directory,
        prefix,
        level,
        filter,
        file_appender,
        max_files,
    } = tracing_args;

    if let Some(backend_type) = backend {
        match backend_type {
            LoggerLayerType::Gelf => {
                tracing.logger.gelf = Some(GelfConfig {
                    addr: addr
                        .ok_or_else(|| eyre!("Gelf backend requires an address."))?
                        .to_socket_addrs()?
                        .next()
                        .ok_or_else(|| eyre!("Invalid gelf address"))?,
                });
            }
            LoggerLayerType::File => {
                let appender_type = match file_appender {
                    Some(LogFileAppenderType::Simple) | None => AppenderType::Simple,
                    Some(LogFileAppenderType::Rolling) => AppenderType::Rolling(RollingConfig {
                        rotation: RotationType::Hourly,
                        retention: RetentionType::None,
                        compression: CompressionType::None,
                    }),
                    Some(LogFileAppenderType::RollingCompressed) => {
                        AppenderType::Rolling(RollingConfig {
                            rotation: RotationType::Hourly,
                            retention: RetentionType::None,
                            compression: CompressionType::Gzip {
                                compression_threshold: Duration::from_hours(2),
                            },
                        })
                    }
                    Some(LogFileAppenderType::RollingMaxFiles) => {
                        AppenderType::Rolling(RollingConfig {
                            rotation: RotationType::Hourly,
                            retention: RetentionType::MaxFiles {
                                max_files: max_files.expect("Max files should be set"),
                            },
                            compression: CompressionType::None,
                        })
                    }
                };
                tracing.logger.file = Some(FileConfig {
                    directory: directory
                        .ok_or_else(|| eyre!("File backend requires a directory."))?,
                    prefix,
                    appender_type,
                });
            }
            LoggerLayerType::Stdout => {
                tracing.logger.stdout = true;
            }
            LoggerLayerType::Stderr => {
                tracing.logger.stderr = true;
            }
        }
    }

    update_tracing_level_and_filter(tracing, level.as_deref(), filter.as_deref())?;

    Ok(())
}

pub fn update_tracing_level_and_filter(
    tracing: &mut TracingConfig,
    level: Option<&str>,
    filter: Option<&str>,
) -> Result<()> {
    if let Some(level) = level {
        tracing.level = level
            .parse()
            .map_err(|_| eyre!("Invalid log level provided: {level}"))?;
    }

    if let Some(filter) = filter {
        tracing.filter = parse_log_filter_layer(filter)?;
    } else {
        apply_default_debug_log_filter(tracing);
    }

    Ok(())
}

pub fn update_tracing_filter_and_derive_level(
    tracing: &mut TracingConfig,
    filter: &str,
) -> Result<()> {
    let layer = parse_log_filter_layer(filter)?;
    let Layer::Env(EnvConfig { ref filters }) = layer else {
        unreachable!("parse_log_filter_layer always returns an env filter");
    };

    if let Some(level) = filters.values().copied().max() {
        tracing.level = level;
    }

    tracing.filter = layer;

    Ok(())
}

/// Parses CLI/env filter overrides into the typed filter config form.
fn parse_log_filter_layer(raw: &str) -> Result<Layer> {
    let filters = parse_filter_directives(raw).map_err(|error| eyre!(error))?;

    Ok(Layer::Env(EnvConfig { filters }))
}

/// Applies the built-in verbose filter policy only when no explicit filter was
/// configured.
fn apply_default_debug_log_filter(tracing: &mut TracingConfig) {
    if !matches!(tracing.filter, Layer::None) {
        return;
    }

    if let Some(filter) = default_envfilter_config(tracing.level) {
        tracing.filter = Layer::Env(EnvConfig {
            filters: filter.filters,
        });
    }
}

pub fn update_network(network: &mut NetworkConfig, network_args: NetworkArgs) -> Result<()> {
    let NetworkArgs {
        host,
        port,
        node_key,
        external_address,
        initial_peers,
    } = network_args;

    if let Some(IpAddr::V4(h)) = host {
        network.backend.swarm.host = h;
    } else if host.is_some() {
        return Err(eyre!("Unsupported ip version"));
    }

    if let Some(port) = port {
        network.backend.swarm.port = port;
    }

    if let Some(node_key) = node_key {
        network.backend.swarm.node_key = node_key;
    }

    if let Some(external_address) = external_address {
        network.backend.swarm.nat = nat::Config::Static { external_address };
    }

    if let Some(peers) = initial_peers {
        network.backend.initial_peers = peers;
    }

    Ok(())
}

pub fn update_blend(blend: &mut BlendConfig, blend_args: BlendArgs) {
    let BlendArgs {
        blend_addr,
        blend_signing_key_id,
        blend_secret_key_id,
    } = blend_args;

    if let Some(addr) = blend_addr {
        blend.set_listening_address(addr);
    }

    if let Some(key_id) = blend_signing_key_id {
        blend.set_non_ephemeral_signing_key_id(key_id);
    }

    if let Some(key_id) = blend_secret_key_id {
        blend.set_secret_zk_key_id(key_id);
    }
}

pub const fn update_cryptarchia(
    cryptarchia: &mut CryptarchiaConfig,
    cryptarchia_args: CryptarchiaArgs,
) {
    let CryptarchiaArgs {
        cryptarchia_funding_pk: funding_pk,
        ..
    } = cryptarchia_args;

    if let Some(pk) = funding_pk {
        cryptarchia.set_funding_pk(pk);
    }
}

pub const fn update_sdp(sdp: &mut SdpConfig, sdp_args: SdpArgs) {
    let SdpArgs {
        sdp_funding_pk: funding_pk,
    } = sdp_args;

    if let Some(pk) = funding_pk {
        sdp.set_funding_pk(pk);
    }
}

pub fn update_api(api: &mut ApiConfig, args: ApiArgs) {
    let ApiArgs { addr, cors_origins } = args;

    if let Some(addr) = addr {
        api.backend.listen_address = addr;
    }

    if let Some(cors) = cors_origins {
        api.backend.cors_origins = cors;
    }
}

pub fn update_state(state: &mut StateConfig, args: StateArgs) {
    let StateArgs { path } = args;

    if let Some(path) = path {
        state.base_folder = path;
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

pub fn parse_hex_public_key(key: &str) -> Result<ZkPublicKey, String> {
    let bytes = hex::decode(key).map_err(|e| format!("Failed to parse hex string: {e}"))?;

    let fr =
        fr_from_bytes(&bytes).map_err(|e| format!("Failed to deserialize Fr from bytes: {e}"))?;

    Ok(ZkPublicKey::new(fr))
}

pub fn parse_hex_zk_key(s: &str) -> Result<UnsecuredZkKey, String> {
    let bytes = hex::decode(s).map_err(|e| format!("Invalid hex string for ZK key: {e}"))?;

    let big_uint = BigUint::from_bytes_le(&bytes);

    Ok(UnsecuredZkKey::from(big_uint))
}

pub fn parse_hex_ed25519_key(key: &str) -> Result<SecretKey, String> {
    let mut key_bytes = hex::decode(key).map_err(|e| format!("Failed to parse hex string: {e}"))?;

    SecretKey::try_from_bytes(key_bytes.as_mut_slice())
        .map_err(|e| format!("Failed to deserialize ed25519 key from bytes: {e}"))
}
