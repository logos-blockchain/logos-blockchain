pub mod api;
pub mod cli;
pub mod config;
pub mod generic_services;
pub mod global_allocators;
pub mod panic;

use std::{collections::HashMap, panic::set_hook, sync::Arc};

use color_eyre::eyre::{Result, eyre};
pub use lb_blend_service::core::backends::libp2p::Libp2pBlendBackend as BlendBackend;
use lb_core::{
    block::Proposal,
    mantle::{ledger::verification_mode::StandardMode, transactions::states::Preverified},
};
pub use lb_core::{
    header::HeaderId,
    mantle::{SignedOps, traits::Hashable, transactions::hash::TxHash},
};
use lb_era_parameters::ProtocolNames;
pub use lb_network_service::backends::libp2p::Libp2p as NetworkBackend;
use lb_storage_service::recovery::load_recovery_data;
pub use lb_storage_service::{
    backend::{SerdeOp, StorageBackend},
    rocksdb::{RocksBackend, RocksBackendSettings},
};
pub use lb_system_sig_service::SystemSig;
use lb_time_service::backends::NtpTimeBackend;
pub use lb_tracing_service::Tracing;
use lb_tx_service::{
    network::adapters::libp2p::MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE,
    storage::adapters::RocksStorageAdapter,
};
pub use lb_tx_service::{
    network::adapters::libp2p::{
        Libp2pAdapter as MempoolNetworkAdapter, Settings as MempoolAdapterSettings,
        Settings as AdapterSettings,
    },
    tx::settings::TxMempoolSettings,
};
use overwatch::{
    DynError, derive_services,
    overwatch::{Error as OverwatchError, Overwatch, OverwatchRunner},
};
use tokio::runtime;

use crate::{
    api::backend::AxumBackend,
    config::{
        RunConfig, api::ServiceConfig as ApiConfig, blend::ServiceConfig as BlendConfig,
        cryptarchia::ServiceConfig as CryptarchiaConfig, kms::ServiceConfig as KmsConfig,
        mempool::ServiceConfig as MempoolConfig, network::ServiceConfig as NetworkConfig,
        pow::ServiceConfig as PoWConfig, sdp::ServiceConfig as SdpConfig,
        storage::ServiceConfig as StorageConfig, time::ServiceConfig as TimeConfig,
        wallet::ServiceConfig as WalletConfig,
    },
    generic_services::{SdpMempoolAdapter, SdpRecoveryBackend, SdpService, SdpWalletAdapter},
    panic::log_and_exit_hook,
};

/// The data limit of every gossip topic of every era. Gossipsub fixes its
/// topics' limits when the swarm is built, so the topics of every scheduled
/// era are registered at startup, each era's ahead of its activation.
fn max_data_size_by_topic<'names>(
    eras: impl IntoIterator<Item = &'names ProtocolNames>,
) -> HashMap<lb_libp2p::gossipsub::TopicHash, usize> {
    let mut limits: HashMap<lb_libp2p::gossipsub::TopicHash, usize> = HashMap::new();
    let topics = eras.into_iter().flat_map(|names| {
        [
            (
                names.mempool_topic.as_str(),
                MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE,
            ),
            (names.cryptarchia_topic.as_str(), Proposal::MAX_ENCODED_SIZE),
        ]
    });
    for (topic, required) in topics {
        let topic = lb_libp2p::gossipsub::IdentTopic::new(topic).hash();
        limits
            .entry(topic)
            .and_modify(|existing| *existing = (*existing).max(required))
            .or_insert(required);
    }
    limits
}
pub use crate::{
    cli::Command,
    config::{ApiArgs, LogArgs, NetworkArgs, UserConfig},
};

pub(crate) type TracingService = Tracing<RuntimeServiceId>;

pub(crate) type NetworkService =
    lb_network_service::NetworkService<NetworkBackend, RuntimeServiceId>;

pub(crate) type BlendCoreService = generic_services::blend::BlendCoreService<RuntimeServiceId>;
pub(crate) type BlendEdgeService = generic_services::blend::BlendEdgeService<RuntimeServiceId>;
pub(crate) type BlendBroadcastService =
    generic_services::blend::BlendBroadcastService<RuntimeServiceId>;
pub(crate) type BlendService = generic_services::blend::BlendService<RuntimeServiceId>;

pub(crate) type BlockBroadcastService =
    lb_chain_broadcast_service::BlockBroadcastService<RuntimeServiceId>;

pub(crate) type MempoolService = generic_services::TxMempoolService<RuntimeServiceId>;

pub(crate) type KeyManagementService = generic_services::KeyManagementService<RuntimeServiceId>;

pub(crate) type WalletService =
    generic_services::WalletService<CryptarchiaService, RuntimeServiceId>;

pub(crate) type CryptarchiaService = generic_services::CryptarchiaService<RuntimeServiceId>;

pub(crate) type ChainNetworkService = generic_services::ChainNetworkService<RuntimeServiceId>;

pub(crate) type CryptarchiaLeaderService = generic_services::CryptarchiaLeaderService<
    CryptarchiaService,
    ChainNetworkService,
    WalletService,
    RuntimeServiceId,
>;

pub type TimeService = generic_services::TimeService<RuntimeServiceId>;

pub type PoWService = generic_services::PoWService<RuntimeServiceId>;

pub type ApiService = lb_api_service::ApiService<
    AxumBackend<
        NtpTimeBackend,
        RocksStorageAdapter<SignedOps<Preverified, StandardMode>, TxHash>,
        SdpMempoolAdapter<RuntimeServiceId>,
        SdpWalletAdapter<RuntimeServiceId>,
        SdpRecoveryBackend<RuntimeServiceId>,
        CryptarchiaLeaderService,
    >,
    RuntimeServiceId,
>;

pub type StorageService = lb_storage_service::StorageService<RuntimeServiceId>;

pub type SystemSigService = SystemSig<RuntimeServiceId>;

#[derive_services]
pub struct LogosBlockchain {
    network: NetworkService,
    blend: BlendService,
    blend_core: BlendCoreService,
    blend_edge: BlendEdgeService,
    blend_broadcast: BlendBroadcastService,
    mempool: MempoolService,
    cryptarchia: CryptarchiaService,
    chain_network: ChainNetworkService,
    cryptarchia_leader: CryptarchiaLeaderService,
    block_broadcast: BlockBroadcastService,
    sdp: SdpService<RuntimeServiceId>,
    pow: PoWService,
    time: TimeService,
    http: ApiService,
    storage: StorageService,
    system_sig: SystemSigService,
    key_management: KeyManagementService,
    wallet: WalletService,

    tracing: TracingService,
}

pub fn run_node_from_config(
    config: RunConfig,
    handle: Option<runtime::Handle>,
) -> Result<Overwatch<RuntimeServiceId>, DynError> {
    // Read before the deployment settings are consumed piecewise below. The
    // chain ID is fixed by the deployment, so the API backend is handed it up
    // front rather than querying a service for a value that cannot change.
    let chain_id = config.deployment.chain_id();

    // The schedule, resolved once and shared by every service, each of which
    // follows the era in force on its own.
    let eras = Arc::new(config.deployment.eras()?);
    // The names the network service starts with. Kademlia's and identify's are
    // the chain's own, the same in every era; the chain sync protocols and the
    // gossip topics follow the era in force, set by the services that use them
    // once they start.
    let protocol_names = eras.genesis().entry.parameters.protocol_names.clone();

    let genesis_block = config.deployment.genesis_block;

    let storage_config = StorageConfig {
        user: config.user.storage,
    }
    .into_rocks_backend_settings(&config.user.state);

    let recovery_data = load_recovery_data(
        storage_config.clone(),
        Arc::new(eras.map(|era| era.entry.parameters.fork_digest)),
    )?;

    let (blend_config, blend_core_config, blend_edge_config) = BlendConfig {
        user: config.user.blend,
    }
    .into_blend_services_settings(&eras, recovery_data.clone());

    let time_service_config = TimeConfig {
        user: config.user.time,
    }
    .into_time_service_settings(&eras);

    let (chain_service_config, chain_network_config, chain_leader_config) = CryptarchiaConfig {
        user: config.user.cryptarchia,
    }
    .into_cryptarchia_services_settings(&eras, genesis_block, recovery_data.clone());

    let mempool_service_config = MempoolConfig {
        user: config.user.mempool,
    }
    .into_mempool_service_settings(&eras, recovery_data.clone());

    let network_service_config = NetworkConfig {
        user: config.user.network,
    }
    .into_network_config(
        &protocol_names,
        max_data_size_by_topic(eras.iter().map(|era| &era.entry.parameters.protocol_names)),
    );

    let wallet_config = WalletConfig {
        user: config.user.wallet,
    }
    .into_wallet_service_settings(recovery_data.clone());

    let kms_config = KmsConfig {
        user: config.user.kms,
    }
    .into();

    let sdp_config = SdpConfig {
        user: config.user.sdp,
    }
    .into_sdp_service_settings(recovery_data.clone());

    let pow_config = PoWConfig {
        user: config.user.pow,
    }
    .into_pow_service_settings(&eras, recovery_data);

    let tracing_config = config::tracing::ServiceConfig {
        user: config.user.tracing,
    }
    .into();

    let api_config = ApiConfig {
        user: config.user.api,
        chain_id,
    };

    let http_config = api_config.backend_settings();

    set_hook(Box::new(log_and_exit_hook));

    let app = OverwatchRunner::<LogosBlockchain>::run(
        LogosBlockchainServiceSettings {
            network: network_service_config,
            blend: blend_config.clone(),
            blend_core: blend_core_config,
            blend_edge: blend_edge_config,
            blend_broadcast: blend_config.map(|era| era.entry.parameters.clone().into()),
            block_broadcast: (),
            mempool: mempool_service_config,
            cryptarchia: chain_service_config,
            chain_network: chain_network_config,
            cryptarchia_leader: chain_leader_config,
            time: time_service_config,
            http: http_config,
            storage: storage_config,
            system_sig: (),
            key_management: kms_config,
            sdp: sdp_config,
            pow: pow_config,
            wallet: wallet_config,

            tracing: tracing_config,
        },
        handle,
    )
    .map_err(|e| eyre!("Error encountered: {}", e))?;
    Ok(app)
}

pub async fn get_services_to_start(
    app: &Overwatch<RuntimeServiceId>,
) -> Result<Vec<RuntimeServiceId>, OverwatchError> {
    let mut service_ids = app.handle().retrieve_service_ids().await?;

    // Exclude core, edge and broadcast blend services, which will be started
    // on demand by the blend orchestrator service.
    let blend_inner_service_ids = [
        RuntimeServiceId::BlendCore,
        RuntimeServiceId::BlendEdge,
        RuntimeServiceId::BlendBroadcast,
    ];
    service_ids.retain(|value| !blend_inner_service_ids.contains(value));

    // Start tracing first so the global subscriber is installed before the
    // rest of the node services spawn their long-running tasks.
    if let Some(index) = service_ids
        .iter()
        .position(|value| *value == RuntimeServiceId::Tracing)
    {
        let tracing = service_ids.remove(index);
        service_ids.insert(0, tracing);
    }

    Ok(service_ids)
}

#[cfg(test)]
mod tests {
    use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};

    use super::*;

    fn names(fork_digest: [u8; 32]) -> ProtocolNames {
        let chain_id = ChainId::try_from("test".to_owned()).unwrap();
        ProtocolNames::derive(&chain_id, ForkDigest::from(fork_digest))
    }

    fn hash(topic: &str) -> lb_libp2p::gossipsub::TopicHash {
        lb_libp2p::gossipsub::IdentTopic::new(topic).hash()
    }

    #[test]
    fn shared_application_topics_use_the_largest_data_limit() {
        let topic = "/shared/application/topic".to_owned();
        let shared = ProtocolNames {
            mempool_topic: topic.clone(),
            cryptarchia_topic: topic.clone(),
            ..names([0; 32])
        };
        let limits = max_data_size_by_topic([&shared]);

        assert_eq!(
            limits.get(&hash(&topic)),
            Some(&MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE.max(Proposal::MAX_ENCODED_SIZE))
        );
    }

    #[test]
    fn the_topics_of_every_era_are_registered() {
        let (genesis, next) = (names([0; 32]), names([1; 32]));
        let limits = max_data_size_by_topic([&genesis, &next]);

        assert_eq!(limits.len(), 4);
        for era in [&genesis, &next] {
            assert_eq!(
                limits.get(&hash(&era.mempool_topic)),
                Some(&MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE)
            );
            assert_eq!(
                limits.get(&hash(&era.cryptarchia_topic)),
                Some(&Proposal::MAX_ENCODED_SIZE)
            );
        }
    }
}
