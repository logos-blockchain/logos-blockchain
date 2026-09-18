use lb_key_management_system_service::backend::preload::KeyId;
use serde::{Deserialize, Serialize};

pub mod leader;
pub mod network;
pub mod service;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub service: service::Config,
    #[serde(default)]
    pub network: network::Config,
    pub leader: leader::Config,
}

pub struct RequiredValues {
    pub funding_key_id: KeyId,
}

impl Config {
    #[must_use]
    pub fn with_required_values(RequiredValues { funding_key_id }: RequiredValues) -> Self {
        Self {
            leader: leader::Config {
                wallet: leader::WalletConfig {
                    funding_key_id,
                    max_tx_fee: leader::default_max_tx_fee(),
                },
            },
            network: network::Config::default(),
            service: service::Config::default(),
        }
    }

    pub fn set_funding_key_id(&mut self, key_id: KeyId) {
        self.leader.wallet.funding_key_id = key_id;
    }
}
