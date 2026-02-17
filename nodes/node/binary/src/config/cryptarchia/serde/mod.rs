use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

use crate::config::utils;

pub mod leader;
pub mod network;
pub mod service;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub leader: leader::Config,

    #[serde(default)]
    #[serde(skip_serializing_if = "utils::is_default")]
    pub service: service::Config,
    #[serde(default)]
    #[serde(skip_serializing_if = "utils::is_default")]
    pub network: network::Config,
}

pub struct RequiredValues {
    pub funding_pk: ZkPublicKey,
}

impl Config {
    #[must_use]
    pub fn with_required_values(RequiredValues { funding_pk }: RequiredValues) -> Self {
        Self {
            leader: leader::Config {
                wallet: leader::WalletConfig {
                    funding_pk,
                    max_tx_fee: leader::default_max_tx_fee(),
                },
            },
            network: network::Config::default(),
            service: service::Config::default(),
        }
    }
}
