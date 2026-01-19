pub mod api;
pub mod config;
pub mod generic_services;

use color_eyre::eyre::{Result, eyre};
use generic_services::{SamplingMempoolAdapter, VerifierMempoolAdapter};
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
pub use lb_da_network_service::backends::libp2p::validator::DaNetworkValidatorBackend;
use lb_da_network_service::{
    DaAddressbook, api::http::HttApiAdapter, membership::handler::DaMembershipHandler,
};
use lb_da_sampling_service::{
    backend::kzgrs::KzgrsSamplingBackend,
    network::adapters::validator::Libp2pAdapter as SamplingLibp2pAdapter,
    storage::adapters::rocksdb::{
        RocksAdapter as SamplingStorageAdapter, converter::DaStorageConverter,
    },
};
use lb_da_verifier_service::{
    backend::kzgrs::KzgrsDaVerifier,
    network::adapters::validator::Libp2pAdapter as VerifierNetworkAdapter,
    storage::adapters::rocksdb::RocksAdapter as VerifierStorageAdapter,
};
use lb_kzgrs_backend::common::share::DaShare;
pub use lb_kzgrs_backend::dispersal::BlobInfo;
use lb_libp2p::PeerId;
pub use lb_network_service::backends::libp2p::Libp2p as NetworkBackend;
use lb_sdp_service::SdpSettings;
pub use lb_storage_service::backends::{
    SerdeOp,
    rocksdb::{RocksBackend, RocksBackendSettings},
};
use lb_subnetworks_assignations::versions::history_aware_refill::HistoryAware;
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

pub use crate::config::{Config, CryptarchiaLeaderArgs, HttpArgs, LogArgs, NetworkArgs};
use crate::{
    api::backend::AxumBackend,
    config::{
        blend::ServiceConfig as BlendConfig, cryptarchia::ServiceConfig as CryptarchiaConfig,
        mempool::ServiceConfig as MempoolConfig, network::ServiceConfig as NetworkConfig,
        time::ServiceConfig as TimeConfig,
    },
    generic_services::{
        DaMembershipAdapter, DaMembershipStorageGeneric, SdpMempoolAdapterGeneric, SdpService,
        SdpServiceAdapterGeneric,
    },
};

pub const DA_TOPIC: &str = "da";
pub const MB16: usize = 1024 * 1024 * 16;

/// Membership used by the DA Network service.
pub type LogosBlockchainDaMembership = HistoryAware<PeerId>;
type DaMembershipStorage = DaMembershipStorageGeneric<RuntimeServiceId>;
pub type DaNetworkApiAdapter =
    HttApiAdapter<DaMembershipHandler<LogosBlockchainDaMembership>, DaAddressbook>;

#[cfg(feature = "tracing")]
pub(crate) type TracingService = Tracing<RuntimeServiceId>;

pub(crate) type NetworkService =
    lb_network_service::NetworkService<NetworkBackend, RuntimeServiceId>;

pub(crate) type DaSamplingAdapter = SamplingLibp2pAdapter<
    LogosBlockchainDaMembership,
    DaMembershipAdapter<RuntimeServiceId>,
    DaMembershipStorage,
    DaNetworkApiAdapter,
    SdpServiceAdapterGeneric<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub(crate) type BlendCoreService =
    generic_services::blend::BlendCoreService<DaSamplingAdapter, RuntimeServiceId>;
pub(crate) type BlendEdgeService =
    generic_services::blend::BlendEdgeService<DaSamplingAdapter, RuntimeServiceId>;
pub(crate) type BlendService =
    generic_services::blend::BlendService<DaSamplingAdapter, RuntimeServiceId>;

pub(crate) type BlockBroadcastService =
    lb_chain_broadcast_service::BlockBroadcastService<RuntimeServiceId>;
pub(crate) type DaVerifierService = generic_services::DaVerifierService<
    VerifierNetworkAdapter<
        LogosBlockchainDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        DaNetworkApiAdapter,
        SdpServiceAdapterGeneric<RuntimeServiceId>,
        RuntimeServiceId,
    >,
    VerifierMempoolAdapter<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub(crate) type DaSamplingService =
    generic_services::DaSamplingService<DaSamplingAdapter, RuntimeServiceId>;

pub(crate) type DaNetworkService = lb_da_network_service::NetworkService<
    DaNetworkValidatorBackend<LogosBlockchainDaMembership>,
    LogosBlockchainDaMembership,
    DaMembershipAdapter<RuntimeServiceId>,
    DaMembershipStorage,
    DaNetworkApiAdapter,
    SdpServiceAdapterGeneric<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub(crate) type MempoolService = generic_services::TxMempoolService<RuntimeServiceId>;

pub(crate) type DaNetworkAdapter =
    lb_da_sampling_service::network::adapters::validator::Libp2pAdapter<
        LogosBlockchainDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        DaNetworkApiAdapter,
        SdpServiceAdapterGeneric<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub(crate) type KeyManagementService = generic_services::KeyManagementService<RuntimeServiceId>;

pub(crate) type WalletService =
    generic_services::WalletService<CryptarchiaService, RuntimeServiceId>;

pub(crate) type CryptarchiaService = generic_services::CryptarchiaService<RuntimeServiceId>;

pub(crate) type ChainNetworkService =
    generic_services::ChainNetworkService<DaNetworkAdapter, RuntimeServiceId>;

pub(crate) type CryptarchiaLeaderService = generic_services::CryptarchiaLeaderService<
    CryptarchiaService,
    WalletService,
    DaNetworkAdapter,
    RuntimeServiceId,
>;

pub type TimeService = generic_services::TimeService<RuntimeServiceId>;

pub type ApiStorageAdapter<RuntimeServiceId> =
    lb_api_service::http::storage::adapters::rocksdb::RocksAdapter<RuntimeServiceId>;

pub type ApiService = lb_api_service::ApiService<
    AxumBackend<
        DaShare,
        LogosBlockchainDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        KzgrsDaVerifier,
        VerifierNetworkAdapter<
            LogosBlockchainDaMembership,
            DaMembershipAdapter<RuntimeServiceId>,
            DaMembershipStorage,
            DaNetworkApiAdapter,
            SdpServiceAdapterGeneric<RuntimeServiceId>,
            RuntimeServiceId,
        >,
        VerifierStorageAdapter<DaShare, DaStorageConverter>,
        DaStorageConverter,
        KzgrsSamplingBackend,
        lb_da_sampling_service::network::adapters::validator::Libp2pAdapter<
            LogosBlockchainDaMembership,
            DaMembershipAdapter<RuntimeServiceId>,
            DaMembershipStorage,
            DaNetworkApiAdapter,
            SdpServiceAdapterGeneric<RuntimeServiceId>,
            RuntimeServiceId,
        >,
        SamplingMempoolAdapter<RuntimeServiceId>,
        SamplingStorageAdapter<DaShare, DaStorageConverter>,
        VerifierMempoolAdapter<RuntimeServiceId>,
        NtpTimeBackend,
        DaNetworkApiAdapter,
        SdpServiceAdapterGeneric<RuntimeServiceId>,
        ApiStorageAdapter<RuntimeServiceId>,
        RocksStorageAdapter<SignedMantleTx, TxHash>,
        SdpMempoolAdapterGeneric<RuntimeServiceId>,
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
    da_verifier: DaVerifierService,
    da_sampling: DaSamplingService,
    da_network: DaNetworkService,
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

pub fn run_node_from_config(config: Config) -> Result<Overwatch<RuntimeServiceId>, DynError> {
    let time_service_config = TimeConfig {
        user: config.time,
        deployment: config.deployment.time,
    }
    .into_time_service_settings(&config.deployment.cryptarchia);

    let (chain_service_config, chain_network_config, chain_leader_config) = CryptarchiaConfig {
        user: config.cryptarchia,
        deployment: config.deployment.cryptarchia,
    }
    .into_cryptarchia_services_settings(&config.deployment.blend);

    let (blend_config, blend_core_config, blend_edge_config) = BlendConfig {
        user: config.blend,
        deployment: config.deployment.blend,
    }
    .into();

    let mempool_service_config = MempoolConfig {
        user: config.mempool,
        deployment: config.deployment.mempool,
    }
    .into();

    let app = OverwatchRunner::<LogosBlockchain>::run(
        LogosBlockchainServiceSettings {
            network: NetworkConfig {
                user: config.network,
                deployment: config.deployment.network,
            }
            .into(),
            blend: blend_config,
            blend_core: blend_core_config,
            blend_edge: blend_edge_config,
            block_broadcast: (),
            #[cfg(feature = "tracing")]
            tracing: config.tracing,
            http: config.http,
            mempool: mempool_service_config,
            da_network: config.da_network,
            da_sampling: config.da_sampling,
            da_verifier: config.da_verifier,
            cryptarchia: chain_service_config,
            chain_network: chain_network_config,
            cryptarchia_leader: chain_leader_config,
            time: time_service_config,
            storage: config.storage,
            system_sig: (),
            key_management: config.key_management,
            sdp: SdpSettings { declaration: None },
            wallet: config.wallet,
            #[cfg(feature = "testing")]
            testing_http: config.testing_http,
        },
        None,
    )
    .map_err(|e| eyre!("Error encountered: {}", e))?;
    Ok(app)
}

pub async fn get_services_to_start(
    app: &Overwatch<RuntimeServiceId>,
    must_blend_service_group_start: bool,
    must_da_service_group_start: bool,
) -> Result<Vec<RuntimeServiceId>, OverwatchError> {
    let mut service_ids = app.handle().retrieve_service_ids().await?;

    // Exclude core and edge blend services, which will be started
    // on demand by the blend service.
    let blend_inner_service_ids = [RuntimeServiceId::BlendCore, RuntimeServiceId::BlendEdge];
    service_ids.retain(|value| !blend_inner_service_ids.contains(value));

    if !must_blend_service_group_start {
        service_ids.retain(|value| value != &RuntimeServiceId::Blend);
    }

    if !must_da_service_group_start {
        let da_service_ids = [
            RuntimeServiceId::DaVerifier,
            RuntimeServiceId::DaSampling,
            RuntimeServiceId::DaNetwork,
            RuntimeServiceId::Mempool,
        ];
        service_ids.retain(|value| !da_service_ids.contains(value));
    }

    Ok(service_ids)
}
