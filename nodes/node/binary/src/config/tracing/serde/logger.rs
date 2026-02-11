use core::net::SocketAddr;
use std::path::PathBuf;

use lb_tracing_service::LoggerLayer;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Layer {
    Gelf(GelfConfig),
    File(FileConfig),
    Loki(LokiConfig),
    Stdout,
    Stderr,
    // do not collect logs
    None,
}

impl From<Layer> for LoggerLayer {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Gelf(config) => {
                LoggerLayer::Gelf(lb_tracing::logging::gelf::GelfConfig { addr: config.addr })
            }
            Layer::File(config) => LoggerLayer::File(lb_tracing::logging::local::FileConfig {
                directory: config.directory,
                prefix: config.prefix,
            }),
            Layer::Loki(config) => LoggerLayer::Loki(lb_tracing::logging::loki::LokiConfig {
                endpoint: config.endpoint,
                host_identifier: config.host_identifier,
            }),
            Layer::Stdout => LoggerLayer::Stdout,
            Layer::Stderr => LoggerLayer::Stderr,
            Layer::None => LoggerLayer::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GelfConfig {
    pub addr: SocketAddr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileConfig {
    pub directory: PathBuf,
    pub prefix: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LokiConfig {
    pub endpoint: Url,
    pub host_identifier: String,
}
