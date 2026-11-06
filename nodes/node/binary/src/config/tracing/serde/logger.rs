use core::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};
use std::path::PathBuf;

pub use lb_tracing::logging::local::{LogFormat, RotationType};
use lb_tracing_service::LoggerLayerSettings;
use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
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
                appender_type: f.appender_type.into(),
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
pub enum AppenderType {
    Simple,
    Rolling(RollingConfig),
}

impl From<AppenderType> for lb_tracing::logging::local::AppenderType {
    fn from(value: AppenderType) -> Self {
        match value {
            AppenderType::Simple => Self::Simple,
            AppenderType::Rolling(config) => Self::Rolling(config.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RollingConfig {
    pub rotation: RotationType,
    pub retention: RetentionType,
    pub compression: CompressionType,
}

impl Default for RollingConfig {
    fn default() -> Self {
        Self {
            rotation: RotationType::Hourly,
            retention: RetentionType::MaxFiles {
                max_files: default_max_files(),
            },
            compression: CompressionType::None,
        }
    }
}

impl From<RollingConfig> for lb_tracing::logging::local::RollingConfig {
    fn from(value: RollingConfig) -> Self {
        Self {
            rotation: value.rotation,
            retention: match value.retention {
                RetentionType::None => lb_tracing::logging::local::RetentionType::None,
                RetentionType::MaxFiles { max_files } => {
                    lb_tracing::logging::local::RetentionType::MaxFiles { max_files }
                }
            },
            compression: match value.compression {
                CompressionType::None => lb_tracing::logging::local::CompressionType::None,
                CompressionType::Gzip {
                    compression_threshold,
                } => lb_tracing::logging::local::CompressionType::Gzip {
                    compression_threshold,
                },
            },
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub enum RetentionType {
    None,
    MaxFiles {
        #[serde(default = "default_max_files")]
        max_files: usize,
    },
}

#[must_use]
pub const fn default_max_files() -> usize {
    10
}

#[serde_as]
#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub enum CompressionType {
    None,
    Gzip {
        #[serde(default = "default_compression_threshold")]
        #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
        compression_threshold: Duration,
    },
}

#[must_use]
pub const fn default_compression_threshold() -> Duration {
    Duration::from_hours(2)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LokiConfig {
    pub endpoint: Url,
    pub host_identifier: String,
}

impl Default for LokiConfig {
    fn default() -> Self {
        Self {
            endpoint: Url::parse("http://localhost:3100").expect("valid URL"),
            host_identifier: super::default_service_name(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub service_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_header: Option<String>,
    pub protocol: lb_tracing::OtlpProtocol,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: super::default_otlp_endpoint(),
            service_name: super::default_service_name(),
            authorization_header: None,
            protocol: lb_tracing::OtlpProtocol::default(),
        }
    }
}
