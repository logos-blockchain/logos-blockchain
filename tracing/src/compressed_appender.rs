use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use flate2::{Compression, write::GzEncoder};

pub struct CompressedRollingAppender {
    inner: tracing_appender::rolling::RollingFileAppender,
    directory: PathBuf,
    prefix: String,
    last_compression: Instant,
    compression_interval: Duration,
}

impl CompressedRollingAppender {
    pub fn new(directory: PathBuf, prefix: &Path, compression_interval: Duration) -> Self {
        let prefix_str = prefix.to_string_lossy().to_string();
        let inner = tracing_appender::rolling::hourly(&directory, &prefix_str);

        Self {
            inner,
            directory,
            prefix: prefix_str,
            last_compression: Instant::now() - compression_interval,
            compression_interval,
        }
    }

    fn try_spawn_compression(&mut self) {
        if self.last_compression.elapsed() >= self.compression_interval {
            self.last_compression = Instant::now();
            self.spawn_compression_task();
        }
    }

    fn spawn_compression_task(&self) {
        let dir = self.directory.clone();
        let prefix = self.prefix.clone();

        std::thread::spawn(move || {
            let Ok(read_dir) = fs::read_dir(&dir) else {
                return;
            };

            for entry in read_dir.flatten() {
                let path = entry.path();

                if path.is_file()
                    && path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                    && path.extension().and_then(|s| s.to_str()) != Some("log")
                    && path.extension().and_then(|s| s.to_str()) != Some("gz")
                {
                    if let Err(e) = compress_file_gzip(&path) {
                        eprintln!("failed to compress logs {}: {e}", path.display());
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
    let output_path = path.with_extension("gz");
    let output = fs::File::create(output_path)?;

    let mut encoder = GzEncoder::new(output, Compression::default());
    let mut reader = io::BufReader::new(input);
    io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}
