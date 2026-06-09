use std::{io::Write, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::fmt::{
    Layer,
    format::{DefaultFields, Format},
};

use crate::compressed_appender::CompressedRollingAppender;

pub type FmtLayer<S> = Layer<S, DefaultFields, Format, tracing_appender::non_blocking::NonBlocking>;

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub enum RetentionType {
    None,
    MaxFiles { max_files: usize },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RotationType {
    Minutely,
    Hourly,
    Daily,
}

impl RotationType {
    #[must_use]
    pub const fn to_rotation(&self) -> Rotation {
        match self {
            Self::Minutely => Rotation::MINUTELY,
            Self::Hourly => Rotation::HOURLY,
            Self::Daily => Rotation::DAILY,
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub enum CompressionType {
    None,
    Gzip { compression_threshold: Duration },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RollingConfig {
    pub rotation: RotationType,
    pub retention: RetentionType,
    pub compression: CompressionType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AppenderType {
    Simple,
    Rolling(RollingConfig),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileConfig {
    pub directory: PathBuf,
    pub prefix: Option<PathBuf>,
    pub appender_type: AppenderType,
}

pub fn create_file_layer<S>(file_config: FileConfig) -> (FmtLayer<S>, WorkerGuard) {
    let prefix = file_config
        .prefix
        .unwrap_or_else(|| "logos-blockchain.log".into());
    let prefix_str = prefix.to_string_lossy().to_string();

    let mut builder = tracing_appender::rolling::Builder::new().filename_prefix(&prefix_str);

    let (rotation, retention, compression) = match &file_config.appender_type {
        AppenderType::Rolling(config) => (
            config.rotation.to_rotation(),
            config.retention,
            config.compression,
        ),
        AppenderType::Simple => (Rotation::NEVER, RetentionType::None, CompressionType::None),
    };

    builder = builder.rotation(rotation);

    if let AppenderType::Rolling(_) = file_config.appender_type {
        builder = builder.latest_symlink(format!("{prefix_str}.latest"));
        if let RetentionType::MaxFiles { max_files } = retention {
            builder = builder.max_log_files(max_files);
        }
    }

    let rolling_appender = builder
        .build(file_config.directory.clone())
        .expect("Failed to initialize rolling appender");

    match compression {
        CompressionType::Gzip {
            compression_threshold,
        } => {
            let appender = CompressedRollingAppender::new(
                rolling_appender,
                file_config.directory,
                prefix_str,
                compression_threshold,
            );
            create_writer_layer(appender)
        }
        CompressionType::None => create_writer_layer(rolling_appender),
    }
}

pub fn create_writer_layer<S, W>(writer: W) -> (FmtLayer<S>, WorkerGuard)
where
    W: Write + Send + 'static,
{
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);

    let layer = Layer::new().with_level(true).with_writer(non_blocking);

    (layer, guard)
}
