use lb_tracing_service::{ConsoleLayer, TokioConsoleConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Layer {
    Console(TokioConfig),
    None,
}

impl From<Layer> for ConsoleLayer {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Console(config) => ConsoleLayer::Console(TokioConsoleConfig {
                bind_address: config.bind_address,
                port: config.port,
            }),
            Layer::None => ConsoleLayer::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokioConfig {
    pub bind_address: String,
    pub port: u16,
}
