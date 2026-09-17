use std::{io::Write, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::fmt::{
    Layer,
    format::{DefaultFields, Format, Json, JsonFields},
};

use crate::compressed_appender::CompressedRollingAppender;

pub type FmtLayer<S> = Layer<S, DefaultFields, Format, tracing_appender::non_blocking::NonBlocking>;
pub type JsonFmtLayer<S> =
    Layer<S, JsonFields, Format<Json>, tracing_appender::non_blocking::NonBlocking>;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

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
    create_file_layer_with_writer(file_config, |writer| create_writer_layer::<S, _>(writer))
}

pub fn create_json_file_layer<S>(file_config: FileConfig) -> (JsonFmtLayer<S>, WorkerGuard) {
    create_file_layer_with_writer(file_config, |writer| {
        create_json_writer_layer::<S, _>(writer)
    })
}

fn create_file_layer_with_writer<L, W>(file_config: FileConfig, create_layer: L) -> (W, WorkerGuard)
where
    L: FnOnce(Box<dyn Write + Send>) -> (W, WorkerGuard),
{
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
            create_layer(Box::new(appender))
        }
        CompressionType::None => create_layer(Box::new(rolling_appender)),
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

pub fn create_json_writer_layer<S, W>(writer: W) -> (JsonFmtLayer<S>, WorkerGuard)
where
    W: Write + Send + 'static,
{
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);

    let layer = Layer::new()
        .json()
        .with_level(true)
        .with_ansi(false)
        .with_writer(non_blocking);

    (layer, guard)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::prelude::*;

    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("buffer lock should not be poisoned")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn json_writer_emits_one_json_record_without_ansi() {
        let buffer = Buffer::default();
        let output = Arc::clone(&buffer.0);
        let (layer, guard) = create_json_writer_layer::<tracing_subscriber::Registry, _>(buffer);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "json-log-test", answer = 42, "hello");
        });
        drop(guard);

        let bytes = output
            .lock()
            .expect("buffer lock should not be poisoned")
            .clone();
        let line = std::str::from_utf8(&bytes).expect("JSON log should be UTF-8");
        let record: serde_json::Value = serde_json::from_str(line).expect("log should be JSON");
        assert_eq!(record["target"], "json-log-test");
        assert_eq!(record["fields"]["message"], "hello");
        assert_eq!(record["fields"]["answer"], 42);
        assert!(!line.contains('\u{1b}'));
    }
}
