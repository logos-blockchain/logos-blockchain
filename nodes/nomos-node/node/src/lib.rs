pub mod api;
pub mod config;
pub mod generic_services;

use color_eyre::eyre::{Result, eyre};
use generic_services::{SamplingMempoolAdapter, VerifierMempoolAdapter};
use kzgrs_backend::common::share::DaShare;
pub use kzgrs_backend::dispersal::BlobInfo;
pub use nomos_blend_service::{
    core::{
        backends::libp2p::Libp2pBlendBackend as BlendBackend,
        network::libp2p::Libp2pAdapter as BlendNetworkAdapter,
    },
    membership::service::Adapter as BlendMembershipAdapter,
};
pub use nomos_core::{
    codec,
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction, TxHash, select::FillSize as FillSizeWithTx},
};
pub use nomos_da_network_service::backends::libp2p::validator::DaNetworkValidatorBackend;
use nomos_da_network_service::{
    DaAddressbook, api::http::HttApiAdapter, membership::handler::DaMembershipHandler,
};
use nomos_da_sampling::{
    backend::kzgrs::KzgrsSamplingBackend,
    network::adapters::validator::Libp2pAdapter as SamplingLibp2pAdapter,
    storage::adapters::rocksdb::{
        RocksAdapter as SamplingStorageAdapter, converter::DaStorageConverter,
    },
};
use nomos_da_verifier::{
    backend::kzgrs::KzgrsDaVerifier,
    network::adapters::validator::Libp2pAdapter as VerifierNetworkAdapter,
    storage::adapters::rocksdb::RocksAdapter as VerifierStorageAdapter,
};
use nomos_libp2p::PeerId;
pub use nomos_network::backends::libp2p::Libp2p as NetworkBackend;
use nomos_sdp::SdpSettings;
pub use nomos_storage::backends::{
    SerdeOp,
    rocksdb::{RocksBackend, RocksBackendSettings},
};
pub use nomos_system_sig::SystemSig;
use nomos_time::backends::NtpTimeBackend;
#[cfg(feature = "tracing")]
pub use nomos_tracing_service::Tracing;
use overwatch::{
    DynError, derive_services,
    overwatch::{Error as OverwatchError, Overwatch, OverwatchRunner},
};
use subnetworks_assignations::versions::history_aware_refill::HistoryAware;
use tx_service::storage::adapters::RocksStorageAdapter;
pub use tx_service::{
    network::adapters::libp2p::{
        Libp2pAdapter as MempoolNetworkAdapter, Settings as MempoolAdapterSettings,
        Settings as AdapterSettings,
    },
    tx::settings::TxMempoolSettings,
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
pub type NomosDaMembership = HistoryAware<PeerId>;
type DaMembershipStorage = DaMembershipStorageGeneric<RuntimeServiceId>;
pub type DaNetworkApiAdapter = HttApiAdapter<DaMembershipHandler<NomosDaMembership>, DaAddressbook>;

#[cfg(feature = "tracing")]
pub(crate) type TracingService = Tracing<RuntimeServiceId>;

pub(crate) type NetworkService = nomos_network::NetworkService<NetworkBackend, RuntimeServiceId>;

pub(crate) type DaSamplingAdapter = SamplingLibp2pAdapter<
    NomosDaMembership,
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

pub(crate) type BlockBroadcastService = broadcast_service::BlockBroadcastService<RuntimeServiceId>;
pub(crate) type DaVerifierService = generic_services::DaVerifierService<
    VerifierNetworkAdapter<
        NomosDaMembership,
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

pub(crate) type DaNetworkService = nomos_da_network_service::NetworkService<
    DaNetworkValidatorBackend<NomosDaMembership>,
    NomosDaMembership,
    DaMembershipAdapter<RuntimeServiceId>,
    DaMembershipStorage,
    DaNetworkApiAdapter,
    SdpServiceAdapterGeneric<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub(crate) type MempoolService = generic_services::TxMempoolService<RuntimeServiceId>;

pub(crate) type DaNetworkAdapter = nomos_da_sampling::network::adapters::validator::Libp2pAdapter<
    NomosDaMembership,
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

pub(crate) type TimeService = generic_services::TimeService<RuntimeServiceId>;

pub(crate) type ApiStorageAdapter<RuntimeServiceId> =
    nomos_api::http::storage::adapters::rocksdb::RocksAdapter<RuntimeServiceId>;

pub(crate) type ApiService = nomos_api::ApiService<
    AxumBackend<
        DaShare,
        NomosDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        KzgrsDaVerifier,
        VerifierNetworkAdapter<
            NomosDaMembership,
            DaMembershipAdapter<RuntimeServiceId>,
            DaMembershipStorage,
            DaNetworkApiAdapter,
            SdpServiceAdapterGeneric<RuntimeServiceId>,
            RuntimeServiceId,
        >,
        VerifierStorageAdapter<DaShare, DaStorageConverter>,
        DaStorageConverter,
        KzgrsSamplingBackend,
        nomos_da_sampling::network::adapters::validator::Libp2pAdapter<
            NomosDaMembership,
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

type StorageService = nomos_storage::StorageService<RocksBackend, RuntimeServiceId>;

type SystemSigService = SystemSig<RuntimeServiceId>;

#[cfg(feature = "testing")]
type TestingApiService<RuntimeServiceId> =
    nomos_api::ApiService<api::testing::backend::TestAxumBackend, RuntimeServiceId>;

#[derive_services]
pub struct Nomos {
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

    let app = OverwatchRunner::<Nomos>::run(
        NomosServiceSettings {
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
