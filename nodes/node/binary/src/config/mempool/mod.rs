use lb_core::mantle::{
    SignedOps,
    ledger::verification_mode::StandardMode,
    traits::Hashable as _,
    transactions::{hash::TxHash, states::Preverified},
};
use lb_services_utils::overwatch::RecoveryData;
use lb_tx_service::{
    TxMempoolSettings, backend::MempoolSettings,
    network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
};

use crate::config::mempool::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_mempool_service_settings(
        self,
        topic: String,
        recovery_data: RecoveryData,
    ) -> TxMempoolSettings<
        MempoolSettings,
        Libp2pNetworkAdapterSettings<TxHash, SignedOps<Preverified, StandardMode>>,
    > {
        TxMempoolSettings {
            network_adapter: Libp2pNetworkAdapterSettings {
                id: SignedOps::<Preverified, StandardMode>::hash,
                topic,
            },
            pool: MempoolSettings {
                tx_ttl: self.user.tx_ttl,
            },
            recovery_data,
        }
    }
}
