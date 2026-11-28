use lb_core::mantle::{traits::Hashable as _, transactions::hash::TxHash};
use lb_cryptarchia_engine::time::EraSchedule;
use lb_libp2p::protocol_name::ProtocolScope;
use lb_services_utils::overwatch::RecoveryData;
use lb_time::era::{EpochLength as _, EraTiming, SlotDuration as _};
use lb_tx_service::{
    TxMempoolSettings,
    backend::{AdmissionLimits, MempoolSettings, PowClaimEvictionSettings, TtlSettings},
    network::adapters::libp2p::{
        EraSettings as Libp2pNetworkAdapterEraSettings, Settings as Libp2pNetworkAdapterSettings,
    },
};

use crate::{
    ValidatedTx,
    config::{
        deployment::{EraDefinition, era::ruleset::EraRuleset},
        mempool::serde::Config,
    },
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the mempool: the user's, and in each era of `eras` the
    /// era's transaction TTL and retirement grace, the era's acceptance window
    /// for `PoW` reward claims, so the mempool and the ledger agree on when a
    /// claim expires, the fork digests it admits, the era's slot timing, the
    /// most bytes a transaction and the data of an Operation may take, and the
    /// era's transaction topic.
    #[must_use]
    pub fn into_mempool_service_settings(
        self,
        eras: &EraSchedule<EraDefinition>,
        recovery_data: &RecoveryData,
    ) -> TxMempoolSettings<MempoolSettings, Libp2pNetworkAdapterSettings<TxHash, ValidatedTx>> {
        TxMempoolSettings {
            network_adapter: Libp2pNetworkAdapterSettings {
                id: ValidatedTx::hash,
                era_schedule: eras.map(|era| {
                    let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                    Libp2pNetworkAdapterEraSettings {
                        topic: ProtocolScope::Fork(era.parameters.fork_digest)
                            .to_string_with_name("mempool"),
                        max_transaction_size: parameters.cryptarchia.max_transactions_size as usize,
                        max_op_data_size: parameters.cryptarchia.max_op_data_size(),
                    }
                }),
            },
            pool: MempoolSettings {
                ttl: eras.map(|era| {
                    let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                    TtlSettings {
                        tx_ttl: parameters.mempool.transaction_ttl(),
                        retirement_grace: parameters.mempool.retirement_grace(),
                    }
                }),
                pow_claim: Some(eras.map(|era| {
                    let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                    PowClaimEvictionSettings {
                        slot_window: parameters.cryptarchia.pow_config.reward.slot_window,
                    }
                })),
                accepted_fork_digests: eras.map(|era| era.parameters.accepted_fork_digests.clone()),
                era_timing: Some(eras.map(|era| EraTiming {
                    slot_duration: era.parameters.slot_duration(),
                    epoch_length: era.parameters.epoch_length(),
                })),
                admission: eras.map(|era| {
                    let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                    AdmissionLimits {
                        max_transaction_size: parameters.cryptarchia.max_transactions_size as usize,
                        max_op_data_size: parameters.cryptarchia.max_op_data_size() as usize,
                    }
                }),
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
