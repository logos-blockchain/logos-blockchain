use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use lb_tracing_service::LoggerLayer;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::utils;

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum Layer {
    Gelf(GelfConfig),
    File(FileConfig),
    Loki(LokiConfig),
    #[default]
    Stdout,
    Stderr,
    // do not collect logs
    None,
}

impl From<Layer> for LoggerLayer {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Gelf(config) => {
                Self::Gelf(lb_tracing::logging::gelf::GelfConfig { addr: config.addr })
            }
            Layer::File(config) => Self::File(lb_tracing::logging::local::FileConfig {
                directory: config.directory,
                prefix: config.prefix,
            }),
            Layer::Loki(config) => Self::Loki(lb_tracing::logging::loki::LokiConfig {
                endpoint: config.endpoint,
                host_identifier: config.host_identifier,
            }),
            Layer::Stdout => Self::Stdout,
            Layer::Stderr => Self::Stderr,
            Layer::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GelfConfig {
    #[serde(skip_serializing_if = "is_default_addr")]
    pub addr: SocketAddr,
}

fn default_addr() -> SocketAddr {
    SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 9_000).into()
}

fn is_default_addr(addr: &SocketAddr) -> bool {
    *addr == default_addr()
}

impl Default for GelfConfig {
    fn default() -> Self {
        Self {
            addr: default_addr(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FileConfig {
    #[serde(skip_serializing_if = "is_default_directory")]
    pub directory: PathBuf,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub prefix: Option<PathBuf>,
}

fn default_directory() -> PathBuf {
    "./logs".into()
}

fn is_default_directory(directory: &PathBuf) -> bool {
    *directory == default_directory()
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            directory: default_directory(),
            prefix: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LokiConfig {
    pub endpoint: Url,
    pub host_identifier: String,
}
