use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use lb_tracing::logging::local::RollingConfig;
pub use lb_tracing::logging::local::{AppenderType, LogFormat};
use lb_tracing_service::LoggerLayerSettings;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Layers {
    pub file: Option<FileConfig>,
    pub loki: Option<LokiConfig>,
    pub gelf: Option<GelfConfig>,
    pub otlp: Option<OtlpConfig>,
    pub format: LogFormat,
    pub stdout: bool,
    pub stderr: bool,
}

impl Default for Layers {
    fn default() -> Self {
        Self {
            file: Some(FileConfig::default()),
            stdout: true,
            stderr: false,
            loki: None,
            gelf: None,
            otlp: None,
            format: LogFormat::default(),
        }
    }
}

impl From<Layers> for LoggerLayerSettings {
    fn from(value: Layers) -> Self {
        Self {
            file: value.file.map(|f| lb_tracing::logging::local::FileConfig {
                directory: f.directory,
                prefix: f.prefix,
                appender_type: f.appender_type,
            }),
            loki: value.loki.map(|l| lb_tracing::logging::loki::LokiConfig {
                endpoint: l.endpoint,
                host_identifier: l.host_identifier,
            }),
            gelf: value
                .gelf
                .map(|g| lb_tracing::logging::gelf::GelfConfig { addr: g.addr }),
            otlp: value
                .otlp
                .map(|o| lb_tracing::logging::otlp::OtlpLoggingConfig {
                    service: lb_tracing::OtlpServiceConfig {
                        url: o.endpoint,
                        service_name: o.service_name,
                        authorization_header: o.authorization_header,
                        protocol: o.protocol,
                    },
                }),
            format: value.format,
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
    pub appender_type: AppenderType,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("."),
            prefix: None,
            appender_type: AppenderType::Rolling(RollingConfig::default()),
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
    pub protocol: lb_tracing::OtlpProtocol,
}
