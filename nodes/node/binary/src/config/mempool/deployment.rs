use serde::{Deserialize, Serialize};

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub pubsub_topic: String,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Mainnet => mainnet_settings(),
            WellKnownDeployment::Testnet => testnet_settings(),
        }
    }
}

fn mainnet_settings() -> Settings {
    Settings {
        pubsub_topic: "/logos-blockchain/mempool/1.0.0".to_owned(),
    }
}

fn testnet_settings() -> Settings {
    mainnet_settings()
}
