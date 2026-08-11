use lb_core::mantle::{
    SignedMantleTx,
    traits::Hashable as _,
    transactions::{hash::TxHash, states::Preverified},
};
use lb_services_utils::overwatch::RecoveryData;
use lb_tx_service::{
    TxMempoolSettings, backend::MempoolSettings,
    network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
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
    ) -> TxMempoolSettings<
        MempoolSettings,
        Libp2pNetworkAdapterSettings<TxHash, SignedMantleTx<Preverified>>,
    > {
        TxMempoolSettings {
            network_adapter: Libp2pNetworkAdapterSettings {
                id: SignedMantleTx::<Preverified>::hash,
                topic: self.deployment.pubsub_topic,
            },
            pool: MempoolSettings {
                tx_ttl: self.deployment.tx_ttl,
            },
            recovery_data,
        }
    }
}
