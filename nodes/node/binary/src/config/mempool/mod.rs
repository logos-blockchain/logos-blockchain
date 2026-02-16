use std::path::PathBuf;

use lb_core::mantle::{SignedMantleTx, Transaction as _, TxHash};
use lb_tx_service::{
    TxMempoolSettings, network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
};

use crate::config::{
    mempool::deployment::Settings as DeploymentSettings,
    recovery::get_path_for_base_folder_and_file_name, storage::serde::Config as StorageConfig,
};

pub mod deployment;

pub struct ServiceConfig {
    pub deployment: DeploymentSettings,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_mempool_service_settings(
        self,
        storage_config: &StorageConfig,
    ) -> TxMempoolSettings<(), Libp2pNetworkAdapterSettings<TxHash, SignedMantleTx>> {
        let recovery_path = get_path_for_base_folder_and_file_name(
            &storage_config.backend.path,
            PathBuf::new()
                .join("mempool")
                .with_file_name("recovery")
                .with_added_extension("json")
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
