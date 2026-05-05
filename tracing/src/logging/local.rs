use std::{io::Write, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::{
    Layer,
    format::{DefaultFields, Format},
};

use crate::compressed_appender::CompressedRollingAppender;

pub type FmtLayer<S> = Layer<S, DefaultFields, Format, tracing_appender::non_blocking::NonBlocking>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AppenderType {
    Rolling,
    RollingCompressed { compression_interval: Duration },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileConfig {
    pub directory: PathBuf,
    pub prefix: Option<PathBuf>,
    pub appender_type: AppenderType,
}

pub fn create_file_layer<S>(config: FileConfig) -> (FmtLayer<S>, WorkerGuard) {
    match config.appender_type {
        AppenderType::Rolling => {
            let file_appender = tracing_appender::rolling::hourly(
                config.directory,
                config
                    .prefix
                    .unwrap_or_else(|| PathBuf::from("logos-blockchain.log")),
            );
            create_writer_layer(file_appender)
        }
        AppenderType::RollingCompressed {
            compression_interval,
        } => {
            let compressed_appender = CompressedRollingAppender::new(
                config.directory,
                &config
                    .prefix
                    .unwrap_or_else(|| "logos-blockchain.log".into()),
                compression_interval,
            );
            create_writer_layer(compressed_appender)
        }
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
