use lb_core::mantle::{
    SignedOps,
    ledger::verification_mode::StandardMode,
    traits::Hashable as _,
    transactions::{hash::TxHash, states::Preverified},
};
use lb_cryptarchia_engine::time::EraSchedule;
use lb_libp2p::protocol_name::ProtocolScope;
use lb_services_utils::overwatch::RecoveryData;
use lb_tx_service::{
    TxMempoolSettings,
    backend::{MempoolSettings, PowClaimEvictionSettings},
    network::adapters::libp2p::{
        EraSettings as Libp2pNetworkAdapterEraSettings, Settings as Libp2pNetworkAdapterSettings,
    },
};

use crate::config::{
    deployment::{EraDefinition, era::ruleset::EraRuleset},
    mempool::serde::Config,
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the mempool: the user's, and in each era of `eras` the
    /// era's acceptance window for `PoW` reward claims, so the mempool and the
    /// ledger agree on when a claim expires, and the era's transaction topic.
    #[must_use]
    pub fn into_mempool_service_settings(
        self,
        eras: &EraSchedule<EraDefinition>,
        recovery_data: &RecoveryData,
    ) -> TxMempoolSettings<
        MempoolSettings,
        Libp2pNetworkAdapterSettings<TxHash, SignedOps<Preverified, StandardMode>>,
    > {
        TxMempoolSettings {
            network_adapter: Libp2pNetworkAdapterSettings {
                id: SignedOps::<Preverified, StandardMode>::hash,
                era_schedule: eras.map(|era| Libp2pNetworkAdapterEraSettings {
                    topic: ProtocolScope::Fork(era.parameters.fork_digest)
                        .to_string_with_name("mempool"),
                }),
            },
            pool: MempoolSettings {
                tx_ttl: self.user.tx_ttl,
                pow_claim: Some(eras.map(|era| {
                    let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                    PowClaimEvictionSettings {
                        slot_window: parameters.cryptarchia.pow_config.reward.slot_window,
                    }
                })),
            },
            seen_items_subscription_channel_size: self
                .user
                .seen_items_subscription_channel_size
                .try_into()
                .unwrap(),
            recovery_data: recovery_data.clone(),
        }
    }
}
