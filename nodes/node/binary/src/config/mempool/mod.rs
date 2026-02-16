use std::path::PathBuf;

use lb_core::mantle::{SignedMantleTx, Transaction as _, TxHash};
use lb_tx_service::{
    TxMempoolSettings, network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
};

use crate::config::{
    mempool::deployment::Settings as DeploymentSettings, state::Config as StateConfig,
};

pub mod deployment;

pub struct ServiceConfig {
    pub deployment: DeploymentSettings,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_mempool_service_settings(
        self,
        state_config: &StateConfig,
    ) -> TxMempoolSettings<(), Libp2pNetworkAdapterSettings<TxHash, SignedMantleTx>> {
        let recovery_path = state_config.get_path_for_recovery_state(
            PathBuf::new()
                .join("mempool")
                .join("recovery")
                .with_extension("json")
                .as_path(),
        );

        TxMempoolSettings {
            network_adapter: Libp2pNetworkAdapterSettings {
                id: SignedMantleTx::hash,
                topic: self.deployment.pubsub_topic,
            },
            pool: (),
            recovery_path,
        }
    }
}
