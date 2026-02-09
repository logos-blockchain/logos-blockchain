use serde::{Deserialize, Serialize};

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub pubsub_topic: String,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Devnet => devnet_settings(),
        }
    }
}

fn devnet_settings() -> Settings {
    Settings {
        pubsub_topic: "/logos-blockchain-devnet/mempool/1.0.0".to_owned(),
    }
}
