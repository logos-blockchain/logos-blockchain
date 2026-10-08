use lb_core::mantle::{
    SignedOps,
    ledger::verification_mode::StandardMode,
    traits::Hashable as _,
    transactions::{hash::TxHash, states::Preverified},
};
use lb_cryptarchia_engine::era::EraSchedule;
use lb_services_utils::overwatch::RecoveryData;
use lb_tx_service::{
    TxMempoolSettings,
    backend::{MempoolSettings, PowClaimEvictionSettings},
    network::adapters::libp2p::Settings as Libp2pNetworkAdapterSettings,
};

use crate::config::{
    deployment::{EraDefinition, ProtocolScope, era::parameters::EraParameters},
    mempool::serde::Config,
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the mempool in every era of `eras`, each on its era's
    /// transaction topic and evicting `PoW` reward claims past its era's
    /// consensus acceptance window, so the mempool and the ledger agree on when
    /// a claim expires.
    #[must_use]
    #[expect(clippy::type_complexity, reason = "TODO: Address this later.")]
    pub fn into_mempool_service_era_schedule(
        self,
        eras: &EraSchedule<EraDefinition>,
        recovery_data: RecoveryData,
    ) -> EraSchedule<
        TxMempoolSettings<
            MempoolSettings,
            Libp2pNetworkAdapterSettings<TxHash, SignedOps<Preverified, StandardMode>>,
        >,
    > {
        eras.map(|era| {
            let EraParameters::V1(parameters) = &era.entry.parameters.parameters;
            TxMempoolSettings {
                network_adapter: Libp2pNetworkAdapterSettings {
                    id: SignedOps::<Preverified, StandardMode>::hash,
                    topic: ProtocolScope::Fork(era.entry.parameters.fork_digest)
                        .to_string_with_name("mempool"),
                },
                pool: MempoolSettings {
                    tx_ttl: self.user.tx_ttl,
                    pow_claim: Some(PowClaimEvictionSettings {
                        slot_window: parameters.cryptarchia.pow_config.reward.slot_window,
                    }),
                },
                seen_items_subscription_channel_size: self
                    .user
                    .seen_items_subscription_channel_size
                    .try_into()
                    .unwrap(),
                recovery_data: recovery_data.clone(),
            }
        })
    }
}
