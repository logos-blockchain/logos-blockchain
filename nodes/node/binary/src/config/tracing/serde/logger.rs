use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use lb_tracing_service::LoggerLayerSettings;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layers {
    pub file: Option<FileConfig>,
    pub loki: Option<LokiConfig>,
    pub gelf: Option<GelfConfig>,
    pub otlp: Option<OtlpConfig>,
    pub stdout: bool,
    pub stderr: bool,
}

impl Default for Layers {
    fn default() -> Self {
        let now = time::OffsetDateTime::now_utc();
        let date_prefix = now.unix_timestamp().to_string();

        Self {
            file: Some(FileConfig {
                directory: PathBuf::from("."),
                prefix: Some(date_prefix.into()),
            }),
            stdout: true,
            stderr: false,
            loki: None,
            gelf: None,
            otlp: None,
        }
    }
}

impl From<Layers> for LoggerLayerSettings {
    fn from(value: Layers) -> Self {
        Self {
            file: value.file.map(|f| lb_tracing::logging::local::FileConfig {
                directory: f.directory,
                prefix: f.prefix,
            }),
            loki: value.loki.map(|l| lb_tracing::logging::loki::LokiConfig {
                endpoint: l.endpoint,
                host_identifier: l.host_identifier,
            }),
            gelf: value
                .gelf
                .map(|g| lb_tracing::logging::gelf::GelfConfig { addr: g.addr }),
            otlp: value.otlp.map(|o| lb_tracing::logging::otlp::OtlpConfig {
                endpoint: o.endpoint,
                service_name: o.service_name,
                authorization_header: o.authorization_header,
            }),
            stdout: value.stdout,
            stderr: value.stderr,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub service_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_header: Option<String>,
}
