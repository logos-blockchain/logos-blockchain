use std::{
    io::Write,
    path::PathBuf,
    sync::{Mutex, PoisonError, TryLockError},
    thread,
    time::{Duration, Instant},
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

const FLUSH_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const FLUSH_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// Flushes and shuts down every appender created so far.
///
/// The registry lock is only tried, never waited on, so the function is safe
/// to call from a panic hook even if the panicking thread holds the lock.
pub fn flush_appenders() {
    let deadline = Instant::now() + FLUSH_LOCK_TIMEOUT;

    loop {
        match APPENDER_GUARDS.try_lock() {
            Ok(mut guards) => {
                drop(std::mem::take(&mut *guards));
                return;
            }
            Err(TryLockError::Poisoned(poisoned)) => {
                drop(std::mem::take(&mut *poisoned.into_inner()));
                return;
            }
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(FLUSH_LOCK_RETRY_INTERVAL);
            }
            Err(TryLockError::WouldBlock) => return,
        }
    }
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
    use std::{
        env, fs, io,
        panic::set_hook,
        process::{Command, Stdio},
        sync::Arc,
    };

    use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

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

    #[test]
    fn panic_hook_flushes_log_file_before_exit() {
        const LOG_DIR_ENV: &str = "LB_TRACING_PANIC_TEST_LOG_DIR";
        const LOG_PREFIX: &str = "tracing-panic-test.log";
        const PANIC_MESSAGE: &str = "This is fine.";
        const EXIT_CODE: i32 = 1;
        const FILLER_LINES: usize = 100;

        if let Some(log_dir) = env::var_os(LOG_DIR_ENV) {
            let layer = create_file_layer(FileConfig {
                directory: PathBuf::from(log_dir),
                prefix: Some(LOG_PREFIX.into()),
                appender_type: AppenderType::Simple,
            });
            tracing_subscriber::registry().with(layer).init();

            set_hook(Box::new(|panic_info| {
                tracing::error!(target: CHILD_TARGET, panic_payload = %panic_info, "A panic occurred");
                flush_appenders();
                std::process::exit(EXIT_CODE);
            }));

            for i in 0..FILLER_LINES {
                tracing::info!(target: CHILD_TARGET, "Fire {i}");
            }

            panic!("{PANIC_MESSAGE}");
        }

        let temp_dir = tempfile::tempdir().expect("temporary directory should exist");
        let current_exe = env::current_exe().expect("test executable should exist");

        let status = Command::new(current_exe)
            .args([
                "--exact",
                "logging::local::tests::panic_hook_flushes_log_file_before_exit",
            ])
            .env(LOG_DIR_ENV, temp_dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("test process should run");

        assert_eq!(status.code(), Some(EXIT_CODE));

        let log = fs::read_to_string(temp_dir.path().join(LOG_PREFIX))
            .expect("should have written the log file");
        assert!(
            log.contains("panic occurred") && log.contains(PANIC_MESSAGE),
            "panic line should have been flushed to the log file: {log}"
        );
        assert_eq!(log.matches("Fire").count(), FILLER_LINES);
    }
}
