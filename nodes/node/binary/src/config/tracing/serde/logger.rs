use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use lb_tracing_service::LoggerLayer;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GelfConfig {
    pub addr: SocketAddr,
}

impl Default for GelfConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 9_000).into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub directory: PathBuf,
    pub prefix: Option<PathBuf>,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            directory: "./logs".into(),
            prefix: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LokiConfig {
    pub endpoint: Url,
    pub host_identifier: String,
}
