use std::sync::Arc;

use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapterSettings;
use lb_core::block::genesis::GenesisBlock;
use lb_cryptarchia_engine::era::Eras;
use lb_era_parameters::{EraDefinition, EraParameters};
use lb_libp2p::PeerId;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::cryptarchia::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_cryptarchia_services_settings(
        self,
        eras: &Eras<EraDefinition>,
        genesis_block: GenesisBlock,
        recovery_data: RecoveryData,
    ) -> (
        lb_chain_service::CryptarchiaSettings,
        lb_chain_network_service::ChainNetworkSettings<PeerId, LibP2pAdapterSettings>,
        lb_chain_leader_service::LeaderSettings,
    ) {
        // The ledger config of every era, which the chain service runs and the
        // leader builds proposals under.
        let ledger_eras = Arc::new(eras.map(|era| match &era.entry.parameters.parameters {
            EraParameters::V1(parameters) => parameters.ledger_config(),
        }));
        let chain_service_settings = lb_chain_service::CryptarchiaSettings {
            bootstrap: lb_chain_service::BootstrapConfig {
                force_bootstrap: self.user.service.bootstrap.force_bootstrap,
                prolonged_bootstrap_period: self.user.service.bootstrap.prolonged_bootstrap_period,
                offline_grace_period: lb_chain_service::OfflineGracePeriodConfig {
                    grace_period: self
                        .user
                        .service
                        .bootstrap
                        .offline_grace_period
                        .grace_period,
                    state_recording_interval: self
                        .user
                        .service
                        .bootstrap
                        .offline_grace_period
                        .state_recording_interval,
                },
            },
            eras: Arc::clone(&ledger_eras),
            recovery_data,
            starting_state: genesis_block.into(),
            sync: lb_chain_service::SyncConfig {
                block_provider: lb_chain_service::BlockProviderConfig {
                    batch_size: self.user.service.sync.block_provider.batch_size,
                },
            },
        };
        let chain_network_settings = lb_chain_network_service::ChainNetworkSettings {
            bootstrap: lb_chain_network_service::BootstrapConfig {
                ibd: lb_chain_network_service::IbdConfig {
                    peers: self.user.network.bootstrap.ibd.peers,
                    tips_fetch_max_attempts: self
                        .user
                        .network
                        .bootstrap
                        .ibd
                        .tips_fetch_max_attempts,
                    tips_fetch_min_delay: self.user.network.bootstrap.ibd.tips_fetch_min_delay,
                    tips_fetch_max_delay: self.user.network.bootstrap.ibd.tips_fetch_max_delay,
                    round_delay: self.user.network.bootstrap.ibd.round_delay,
                },
            },
            network: LibP2pAdapterSettings {
                topics: Arc::new(eras.map(|era| {
                    era.entry
                        .parameters
                        .protocol_names
                        .cryptarchia_topic
                        .clone()
                })),
                max_connected_peers_to_try_download: self
                    .user
                    .network
                    .network
                    .max_connected_peers_to_try_download,
                max_discovered_peers_to_try_download: self
                    .user
                    .network
                    .network
                    .max_discovered_peers_to_try_download,
            },
            sync: lb_chain_network_service::SyncConfig {
                orphan: lb_chain_network_service::OrphanConfig {
                    max_orphan_cache_size: self.user.network.sync.orphan.max_orphan_cache_size,
                    max_rejected_cache_size: self.user.network.sync.orphan.max_rejected_cache_size,
                },
                tip_poll: lb_chain_network_service::TipPollConfig {
                    enabled: self.user.network.sync.tip_poll.enabled,
                    lag_threshold_blocks: self.user.network.sync.tip_poll.lag_threshold_blocks,
                    max_peers_to_sample: self.user.network.sync.tip_poll.max_peers_to_sample,
                },
            },
        };
        let chain_leader_settings = lb_chain_leader_service::LeaderSettings {
            eras: ledger_eras,
            wallet_config: lb_chain_leader_service::LeaderWalletConfig {
                funding_pk: self.user.leader.wallet.funding_pk,
                max_tx_fee: self.user.leader.wallet.max_tx_fee,
            },
        };
        (
            chain_service_settings,
            chain_network_settings,
            chain_leader_settings,
        )
    }
}
