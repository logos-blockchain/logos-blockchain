use core::num::NonZeroU64;

use lb_core::mantle::{
    SignedOps,
    ledger::verification_mode::StandardMode,
    traits::Hashable as _,
    transactions::{hash::TxHash, states::Preverified},
};
use lb_services_utils::overwatch::RecoveryData;
use lb_tx_service::{
    TxMempoolSettings,
    backend::{MempoolSettings, PowClaimEvictionSettings},
    network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
};

use crate::config::mempool::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// `pow_slot_window` is the consensus acceptance window for `PoW` reward
    /// claims, sourced from the cryptarchia deployment configuration so the
    /// mempool and the ledger agree on when a claim expires.
    #[must_use]
    pub fn into_mempool_service_settings(
        self,
        topic: String,
        recovery_data: RecoveryData,
        pow_slot_window: NonZeroU64,
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
                pow_claim: Some(PowClaimEvictionSettings {
                    slot_window: pow_slot_window,
                }),
            },
            seen_items_subscription_channel_size: self
                .user
                .seen_items_subscription_channel_size
                .try_into()
                .unwrap(),
            recovery_data,
        }
    }
}
