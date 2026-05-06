use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use flate2::{Compression, write::GzEncoder};
use tracing_appender::rolling::{RollingFileAppender, Rotation};

struct CompressionGuard(Arc<AtomicBool>);
impl Drop for CompressionGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub struct CompressedRollingAppender {
    inner: RollingFileAppender,
    directory: PathBuf,
    prefix: String,
    last_compression: Instant,
    compression_interval: Duration,
    is_compressing: Arc<AtomicBool>,
}

impl CompressedRollingAppender {
    pub fn new(directory: PathBuf, prefix: &Path, compression_interval: Duration) -> Self {
        let prefix_str = prefix.to_string_lossy().to_string();
        let symlink_name = format!("{prefix_str}.latest");

        let inner = RollingFileAppender::builder()
            .rotation(Rotation::HOURLY)
            .filename_prefix(&prefix_str)
            .latest_symlink(&symlink_name)
            .build(&directory)
            .expect("Failed to initialize rolling appender");

        Self {
            inner,
            directory,
            prefix: prefix_str,
            last_compression: Instant::now() - compression_interval,
            compression_interval,
            is_compressing: Arc::new(AtomicBool::new(false)),
        }
    }

    fn try_spawn_compression(&mut self) {
        if self.last_compression.elapsed() >= self.compression_interval
            && self
                .is_compressing
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            self.last_compression = Instant::now();
            self.spawn_compression_task();
        }
    }

    fn spawn_compression_task(&self) {
        let dir = self.directory.clone();
        let prefix = self.prefix.clone();
        let is_compressing = Arc::clone(&self.is_compressing);
        let symlink_path = dir.join(format!("{prefix}.latest"));

        std::thread::spawn(move || {
            let _guard = CompressionGuard(is_compressing);

            // Resolve the symlink to the actual current log file path.
            let active_file_path = fs::read_link(&symlink_path)
                .ok()
                .map(|p| if p.is_relative() { dir.join(p) } else { p });

            let Ok(read_dir) = fs::read_dir(&dir) else {
                return;
            };

            for entry in read_dir.flatten() {
                let path = entry.path();

                let is_log = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| name.starts_with(&prefix));

                let is_compressed = path.extension().is_some_and(|ext| ext == "gz");
                let is_symlink = path == symlink_path;

                // Check if this file is the one the symlink points to.
                let is_active = active_file_path
                    .as_ref()
                    .is_some_and(|active| active == &path);

                if path.is_file() && is_log && !is_compressed && !is_symlink && !is_active {
                    if let Err(e) = compress_file_gzip(&path) {
                        eprintln!("failed to compress {}: {e}", path.display());
                    } else {
                        drop(fs::remove_file(path));
                    }
                }
            }
        });
    }
}

impl Write for CompressedRollingAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.try_spawn_compression();
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()?;
        Ok(())
    }
}

fn compress_file_gzip(path: &Path) -> io::Result<()> {
    let input = fs::File::open(path)?;
    let mut new_name = path.to_path_buf();
    let ext = path.extension().unwrap_or_default().to_string_lossy();
    new_name.set_extension(format!("{ext}.gz"));

    let output = fs::File::create(new_name)?;
    let mut encoder = GzEncoder::new(output, Compression::default());
    let mut reader = io::BufReader::new(input);
    io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}
