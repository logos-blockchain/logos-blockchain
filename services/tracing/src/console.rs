use std::{fs, io, net::SocketAddr, path::Path};

use console_subscriber::ConsoleLayer;
use tracing_subscriber::Layer;

use crate::TokioConsoleConfig;

pub fn create_console_layer<S>(
    config: &TokioConsoleConfig,
) -> Result<Option<Box<dyn Layer<S> + Send + Sync>>, io::Error>
where
    S: tracing::Subscriber
        + for<'span> tracing_subscriber::registry::LookupSpan<'span>
        + Send
        + Sync,
{
    let bind_addr = format!("{}:{}", config.bind_address, config.port);

    bind_addr.parse::<SocketAddr>().map_or_else(
        |_| Ok(None),
        |socket_addr| {
            let mut builder = ConsoleLayer::builder().server_addr(socket_addr);
            if let Some(recording_path) = &config.recording_path {
                prepare_recording_path(recording_path)?;
                builder = builder.recording_path(recording_path);
            }
            Ok(Some(
                Box::new(builder.spawn()) as Box<dyn Layer<S> + Send + Sync>
            ))
        },
    )
}

fn prepare_recording_path(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            io::Error::new(
                source.kind(),
                format!(
                    "failed to create Tokio console recording directory `{}`: {source}",
                    path.display()
                ),
            )
        })?;
    }

    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| {
            io::Error::new(
                source.kind(),
                format!(
                    "failed to open Tokio console recording path `{}`: {source}",
                    path.display()
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Duration};

    use console_subscriber::ConsoleLayer;
    use tracing::Level;
    use tracing_subscriber::layer::SubscriberExt as _;

    // Running this requires the same `--cfg tokio_unstable` flag as a node
    // binary built with the Tokio Console feature.
    #[ignore = "requires RUSTFLAGS=--cfg tokio_unstable"]
    #[test]
    fn configured_recorder_creates_non_empty_raw_recording() {
        let path =
            std::env::temp_dir().join(format!("logos-tokio-console-{}.jsonl", std::process::id()));
        drop(fs::remove_file(&path));

        let (layer, _server) = ConsoleLayer::builder().recording_path(&path).build();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::span!(target: "tokio", Level::INFO, "runtime.test");
            let _entered = span.enter();
        });

        let non_empty = (0..20).any(|_| {
            thread::sleep(Duration::from_millis(50));
            fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 0)
        });
        assert!(non_empty);
        fs::remove_file(path).expect("test recording should be removable");
    }
}
