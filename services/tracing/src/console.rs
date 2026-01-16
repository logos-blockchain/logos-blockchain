use std::net::SocketAddr;

#[cfg(feature = "profiling")]
use console_subscriber::ConsoleLayer;
use tracing_subscriber::Layer;

use crate::TokioConsoleConfig;

pub fn create_console_layer<S>(
    config: &TokioConsoleConfig,
) -> Option<Box<impl Layer<S> + Send + Sync>>
where
    S: tracing::Subscriber
        + for<'span> tracing_subscriber::registry::LookupSpan<'span>
        + Send
        + Sync,
{
    let bind_addr = format!("{}:{}", config.bind_address, config.port);

    bind_addr.parse::<SocketAddr>().map_or_else(
        |_| None,
        |socket_addr| {
            let console_layer = ConsoleLayer::builder().server_addr(socket_addr).spawn();
            Some(Box::new(console_layer))
        },
    )
}
