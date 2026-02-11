pub mod api;
pub mod config;
pub mod generic_services;
#[cfg(feature = "config-gen")]
pub mod init;

use color_eyre::eyre::{Result, eyre};
pub use lb_blend_service::{
    core::{
        backends::libp2p::Libp2pBlendBackend as BlendBackend,
        network::libp2p::Libp2pAdapter as BlendNetworkAdapter,
    },
    membership::service::Adapter as BlendMembershipAdapter,
};
pub use lb_core::{
    codec,
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction, TxHash, select::FillSize as FillSizeWithTx},
};
pub use lb_network_service::backends::libp2p::Libp2p as NetworkBackend;
pub use lb_storage_service::backends::{
    SerdeOp,
    rocksdb::{RocksBackend, RocksBackendSettings},
};
pub use lb_system_sig_service::SystemSig;
use lb_time_service::backends::NtpTimeBackend;
#[cfg(feature = "tracing")]
pub use lb_tracing_service::Tracing;
use lb_tx_service::storage::adapters::RocksStorageAdapter;
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

pub use crate::config::{HttpArgs, LogArgs, NetworkArgs, UserConfig};
use crate::{
    api::backend::AxumBackend,
    config::{
        RunConfig, blend::ServiceConfig as BlendConfig,
        cryptarchia::ServiceConfig as CryptarchiaConfig, mempool::ServiceConfig as MempoolConfig,
        network::ServiceConfig as NetworkConfig, time::ServiceConfig as TimeConfig,
    },
    generic_services::{SdpMempoolAdapter, SdpService, SdpWalletAdapter},
};

pub const MB16: usize = 1024 * 1024 * 16;

#[cfg(feature = "tracing")]
pub(crate) type TracingService = Tracing<RuntimeServiceId>;

pub(crate) type NetworkService =
    lb_network_service::NetworkService<NetworkBackend, RuntimeServiceId>;

pub(crate) type BlendCoreService = generic_services::blend::BlendCoreService<RuntimeServiceId>;
pub(crate) type BlendEdgeService = generic_services::blend::BlendEdgeService<RuntimeServiceId>;
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

pub type ApiStorageAdapter<RuntimeServiceId> =
    lb_api_service::http::storage::adapters::rocksdb::RocksAdapter<RuntimeServiceId>;

pub type ApiService = lb_api_service::ApiService<
    AxumBackend<
        NtpTimeBackend,
        ApiStorageAdapter<RuntimeServiceId>,
        RocksStorageAdapter<SignedMantleTx, TxHash>,
        SdpMempoolAdapter<RuntimeServiceId>,
        SdpWalletAdapter<RuntimeServiceId>,
        CryptarchiaLeaderService,
    >,
    RuntimeServiceId,
>;

pub type StorageService = lb_storage_service::StorageService<RocksBackend, RuntimeServiceId>;

pub type SystemSigService = SystemSig<RuntimeServiceId>;

#[cfg(feature = "testing")]
type TestingApiService<RuntimeServiceId> =
    lb_api_service::ApiService<api::testing::backend::TestAxumBackend, RuntimeServiceId>;

#[derive_services]
pub struct LogosBlockchain {
    #[cfg(feature = "tracing")]
    tracing: TracingService,
    network: NetworkService,
    blend: BlendService,
    blend_core: BlendCoreService,
    blend_edge: BlendEdgeService,
    mempool: MempoolService,
    cryptarchia: CryptarchiaService,
    chain_network: ChainNetworkService,
    cryptarchia_leader: CryptarchiaLeaderService,
    block_broadcast: BlockBroadcastService,
    sdp: SdpService<RuntimeServiceId>,
    time: TimeService,
    http: ApiService,
    storage: StorageService,
    system_sig: SystemSigService,
    key_management: KeyManagementService,
    wallet: WalletService,
    #[cfg(feature = "testing")]
    testing_http: TestingApiService<RuntimeServiceId>,
}

pub fn run_node_from_config(config: RunConfig) -> Result<Overwatch<RuntimeServiceId>, DynError> {
    let time_service_config = TimeConfig {
        user: config.user.time,
        deployment: config.deployment.time,
    }
    .into_time_service_settings(&config.deployment.cryptarchia);

    let (chain_service_config, chain_network_config, chain_leader_config) = CryptarchiaConfig {
        user: config.user.cryptarchia,
        deployment: config.deployment.cryptarchia,
    }
    .into_cryptarchia_services_settings(&config.deployment.blend);

    let (blend_config, blend_core_config, blend_edge_config) = BlendConfig {
        user: config.user.blend,
        deployment: config.deployment.blend,
    }
    .into();

    let mempool_service_config = MempoolConfig {
        user: config.user.mempool,
        deployment: config.deployment.mempool,
    }
    .into();

    let app = OverwatchRunner::<LogosBlockchain>::run(
        LogosBlockchainServiceSettings {
            network: NetworkConfig {
                user: config.user.network,
                deployment: config.deployment.network,
            }
            .into(),
            blend: blend_config,
            blend_core: blend_core_config,
            blend_edge: blend_edge_config,
            block_broadcast: (),
            #[cfg(feature = "tracing")]
            tracing: config.user.tracing,
            http: config.user.http,
            mempool: mempool_service_config,
            cryptarchia: chain_service_config,
            chain_network: chain_network_config,
            cryptarchia_leader: chain_leader_config,
            time: time_service_config,
            storage: config.user.storage,
            system_sig: (),
            key_management: config.user.key_management,
            sdp: config.user.sdp,
            wallet: config.user.wallet,
            #[cfg(feature = "testing")]
            testing_http: config.user.testing_http,
        },
        None,
    )
    .map_err(|e| eyre!("Error encountered: {}", e))?;
    Ok(app)
}

pub async fn get_services_to_start(
    app: &Overwatch<RuntimeServiceId>,
) -> Result<Vec<RuntimeServiceId>, OverwatchError> {
    let mut service_ids = app.handle().retrieve_service_ids().await?;

    // Exclude core and edge blend services, which will be started
    // on demand by the blend service.
    let blend_inner_service_ids = [RuntimeServiceId::BlendCore, RuntimeServiceId::BlendEdge];
    service_ids.retain(|value| !blend_inner_service_ids.contains(value));

    Ok(service_ids)
}
