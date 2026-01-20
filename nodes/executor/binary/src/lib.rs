pub mod api;
pub mod config;

use api::backend::AxumBackend;
use lb_core::mantle::{SignedMantleTx, TxHash};
use lb_da_dispersal_service::{
    DispersalService,
    adapters::{
        network::libp2p::Libp2pNetworkAdapter as DispersalNetworkAdapter,
        wallet::mock::MockWalletAdapter as DispersalWalletAdapter,
    },
    backend::kzgrs::DispersalKZGRSBackend,
};
use lb_da_network_service::backends::libp2p::executor::DaNetworkExecutorBackend;
use lb_da_sampling_service::{
    backend::kzgrs::KzgrsSamplingBackend,
    storage::adapters::rocksdb::{
        RocksAdapter as SamplingStorageAdapter, converter::DaStorageConverter,
    },
};
use lb_da_verifier_service::{
    backend::kzgrs::KzgrsDaVerifier,
    network::adapters::executor::Libp2pAdapter as VerifierNetworkAdapter,
    storage::adapters::rocksdb::RocksAdapter as VerifierStorageAdapter,
};
use lb_kzgrs_backend::common::share::DaShare;
#[cfg(feature = "tracing")]
use lb_node::Tracing;
use lb_node::{
    BlobInfo, DaNetworkApiAdapter, LogosBlockchainDaMembership, NetworkBackend, RocksBackend,
    SystemSig,
    generic_services::{
        DaMembershipAdapter, DaMembershipStorageGeneric, SamplingMempoolAdapter,
        SdpMempoolAdapterGeneric, SdpService, SdpServiceAdapterGeneric, VerifierMempoolAdapter,
    },
};
use lb_time_service::backends::NtpTimeBackend;
use lb_tx_service::storage::adapters::RocksStorageAdapter;
use overwatch::derive_services;

#[cfg(feature = "tracing")]
pub(crate) type TracingService = Tracing<RuntimeServiceId>;

type DaMembershipStorage = DaMembershipStorageGeneric<RuntimeServiceId>;

pub(crate) type NetworkService =
    lb_network_service::NetworkService<NetworkBackend, RuntimeServiceId>;

pub(crate) type BlendCoreService =
    lb_node::generic_services::blend::BlendCoreService<DaNetworkAdapter, RuntimeServiceId>;
pub(crate) type BlendEdgeService =
    lb_node::generic_services::blend::BlendEdgeService<DaNetworkAdapter, RuntimeServiceId>;
pub(crate) type BlendService =
    lb_node::generic_services::blend::BlendService<DaNetworkAdapter, RuntimeServiceId>;

pub(crate) type BlockBroadcastService =
    lb_chain_broadcast_service::BlockBroadcastService<RuntimeServiceId>;

pub(crate) type DaDispersalService = DispersalService<
    DispersalKZGRSBackend<
        DispersalNetworkAdapter<
            LogosBlockchainDaMembership,
            DaMembershipAdapter<RuntimeServiceId>,
            DaMembershipStorage,
            DaNetworkApiAdapter,
            SdpServiceAdapterGeneric<RuntimeServiceId>,
            RuntimeServiceId,
        >,
        DispersalWalletAdapter,
    >,
    DispersalNetworkAdapter<
        LogosBlockchainDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        DaNetworkApiAdapter,
        SdpServiceAdapterGeneric<RuntimeServiceId>,
        RuntimeServiceId,
    >,
    LogosBlockchainDaMembership,
    RuntimeServiceId,
>;

pub(crate) type DaVerifierService = lb_node::generic_services::DaVerifierService<
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
    lb_node::generic_services::DaSamplingService<DaNetworkAdapter, RuntimeServiceId>;

pub(crate) type DaNetworkService = lb_da_network_service::NetworkService<
    DaNetworkExecutorBackend<LogosBlockchainDaMembership>,
    LogosBlockchainDaMembership,
    DaMembershipAdapter<RuntimeServiceId>,
    DaMembershipStorage,
    DaNetworkApiAdapter,
    SdpServiceAdapterGeneric<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub(crate) type MempoolService = lb_node::generic_services::TxMempoolService<RuntimeServiceId>;

pub(crate) type DaNetworkAdapter =
    lb_da_sampling_service::network::adapters::executor::Libp2pAdapter<
        LogosBlockchainDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        DaNetworkApiAdapter,
        SdpServiceAdapterGeneric<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub(crate) type CryptarchiaService =
    lb_node::generic_services::CryptarchiaService<RuntimeServiceId>;

pub(crate) type ChainNetworkService =
    lb_node::generic_services::ChainNetworkService<DaNetworkAdapter, RuntimeServiceId>;

pub(crate) type WalletService =
    lb_node::generic_services::WalletService<CryptarchiaService, RuntimeServiceId>;

pub(crate) type KeyManagementService =
    lb_node::generic_services::KeyManagementService<RuntimeServiceId>;

pub(crate) type CryptarchiaLeaderService = lb_node::generic_services::CryptarchiaLeaderService<
    CryptarchiaService,
    WalletService,
    DaNetworkAdapter,
    RuntimeServiceId,
>;

pub(crate) type TimeService = lb_node::generic_services::TimeService<RuntimeServiceId>;

pub(crate) type ApiStorageAdapter<RuntimeServiceId> =
    lb_api_service::http::storage::adapters::rocksdb::RocksAdapter<RuntimeServiceId>;

pub(crate) type ApiService = lb_api_service::ApiService<
    AxumBackend<
        DaShare,
        BlobInfo,
        LogosBlockchainDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        BlobInfo,
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
        DispersalKZGRSBackend<
            DispersalNetworkAdapter<
                LogosBlockchainDaMembership,
                DaMembershipAdapter<RuntimeServiceId>,
                DaMembershipStorage,
                DaNetworkApiAdapter,
                SdpServiceAdapterGeneric<RuntimeServiceId>,
                RuntimeServiceId,
            >,
            DispersalWalletAdapter,
        >,
        DispersalNetworkAdapter<
            LogosBlockchainDaMembership,
            DaMembershipAdapter<RuntimeServiceId>,
            DaMembershipStorage,
            DaNetworkApiAdapter,
            SdpServiceAdapterGeneric<RuntimeServiceId>,
            RuntimeServiceId,
        >,
        lb_kzgrs_backend::dispersal::Metadata,
        KzgrsSamplingBackend,
        DaNetworkAdapter,
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

pub(crate) type StorageService = lb_storage_service::StorageService<RocksBackend, RuntimeServiceId>;

pub(crate) type SystemSigService = SystemSig<RuntimeServiceId>;

#[cfg(feature = "testing")]
type TestingApiService<RuntimeServiceId> =
    lb_api_service::ApiService<api::testing::backend::TestAxumBackend, RuntimeServiceId>;

#[derive_services]
pub struct LogosBlockchainExecutor {
    #[cfg(feature = "tracing")]
    tracing: TracingService,
    network: NetworkService,
    blend: BlendService,
    blend_core: BlendCoreService,
    blend_edge: BlendEdgeService,
    da_dispersal: DaDispersalService,
    da_verifier: DaVerifierService,
    da_sampling: DaSamplingService,
    da_network: DaNetworkService,
    sdp: SdpService<RuntimeServiceId>,
    mempool: MempoolService,
    cryptarchia: CryptarchiaService,
    chain_network: ChainNetworkService,
    cryptarchia_leader: CryptarchiaLeaderService,
    block_broadcast: BlockBroadcastService,
    time: TimeService,
    http: ApiService,
    storage: StorageService,
    system_sig: SystemSigService,
    wallet: WalletService,
    key_management: KeyManagementService,
    #[cfg(feature = "testing")]
    testing_http: TestingApiService<RuntimeServiceId>,
}
