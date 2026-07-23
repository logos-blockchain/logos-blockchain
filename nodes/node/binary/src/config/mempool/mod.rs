use lb_core::mantle::{
    SignedMantleTx, Transaction as _, TxHash, transactions::states::Preverified,
};
use lb_services_utils::overwatch::RecoveryData;
use lb_tx_service::{
    TxMempoolSettings, network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
};

use crate::config::mempool::deployment::Settings as DeploymentSettings;

pub mod deployment;

pub struct ServiceConfig {
    pub deployment: DeploymentSettings,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_mempool_service_settings(
        self,
        recovery_data: RecoveryData,
    ) -> TxMempoolSettings<(), Libp2pNetworkAdapterSettings<TxHash, SignedMantleTx<Preverified>>>
    {
        TxMempoolSettings {
            network_adapter: Libp2pNetworkAdapterSettings {
                id: SignedMantleTx::<Preverified>::hash,
                topic: self.deployment.pubsub_topic,
            },
            pool: (),
            recovery_data,
        }
    }
}
