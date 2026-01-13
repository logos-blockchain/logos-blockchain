use nomos_core::mantle::{SignedMantleTx, Transaction as _, TxHash};
use tx_service::{
    TxMempoolSettings, network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
};

use crate::config::mempool::{deployment::Settings as DeploymentSettings, serde::Config};

pub mod deployment;
pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
    pub deployment: DeploymentSettings,
}

impl From<ServiceConfig>
    for TxMempoolSettings<(), Libp2pNetworkAdapterSettings<TxHash, SignedMantleTx>>
{
    fn from(value: ServiceConfig) -> Self {
        Self {
            network_adapter: Libp2pNetworkAdapterSettings {
                id: SignedMantleTx::hash,
                topic: value.deployment.pubsub_topic,
            },
            pool: (),
            recovery_path: value.user.recovery_path,
        }
    }
}
