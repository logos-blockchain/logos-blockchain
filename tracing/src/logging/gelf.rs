use std::{error::Error, net::SocketAddr, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;

const GELF_RECONNECT_INTERVAL: u64 = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GelfConfig {
    pub addr: SocketAddr,
}

pub fn create_gelf_layer(
    config: &GelfConfig,
    handle: &Handle,
) -> Result<tracing_gelf::Logger, Box<dyn Error + Send + Sync>> {
    let (layer, mut task) = tracing_gelf::Logger::builder()
        .connect_tcp(config.addr)
        .expect("Connect to the graylog instance");

    handle.spawn(async move {
        loop {
            if task.connect().await.0.is_empty() {
                break;
            }
            eprintln!("Failed to connect to graylog");
            let delay = Duration::from_secs(GELF_RECONNECT_INTERVAL);
            tokio::time::sleep(delay).await;
        }
    });

    Ok(layer)
}
