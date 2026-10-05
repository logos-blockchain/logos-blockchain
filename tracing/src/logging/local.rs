use std::{
    io::Write,
    path::PathBuf,
    sync::{Mutex, PoisonError, TryLockError},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::fmt::{
    Layer,
    format::{DefaultFields, Format},
};

use crate::compressed_appender::CompressedRollingAppender;

pub type FmtLayer<S> = Layer<S, DefaultFields, Format, tracing_appender::non_blocking::NonBlocking>;

/// Worker guards of every non-blocking appender created by this module.
static APPENDER_GUARDS: Mutex<Vec<WorkerGuard>> = Mutex::new(Vec::new());

/// Flushes and shuts down every appender created so far.
///
/// The registry lock is only tried, never waited on, so the function is safe
/// to call from a panic hook even if the panicking thread holds the lock.
pub fn flush_appenders() {
    let guards = match APPENDER_GUARDS.try_lock() {
        Ok(mut guards) => std::mem::take(&mut *guards),
        Err(TryLockError::Poisoned(poisoned)) => std::mem::take(&mut *poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => return,
    };
    drop(guards);
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

#[must_use]
pub fn create_file_layer<S>(file_config: FileConfig) -> FmtLayer<S> {
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

pub fn create_writer_layer<S, W>(writer: W) -> FmtLayer<S>
where
    W: Write + Send + 'static,
{
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);

    APPENDER_GUARDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(guard);

    Layer::new().with_level(true).with_writer(non_blocking)
}

#[cfg(test)]
mod tests {
    use std::{io, sync::Arc};

    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    const CHILD_TARGET: &str = "lb_tracing::local::tests";

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn flush_appenders_drains() {
        let buffer = SharedBuffer::default();
        let subscriber = tracing_subscriber::registry().with(create_writer_layer(buffer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: CHILD_TARGET, "This is fine");
        });

        flush_appenders();
        // A second call must find nothing to do and not block or panic.
        flush_appenders();

        let contents = buffer.contents();
        assert!(
            contents.contains("This is fine"),
            "should have reached the writer: {contents:?}"
        );
        assert_eq!(contents.matches("This is fine").count(), 1);
    }
}
