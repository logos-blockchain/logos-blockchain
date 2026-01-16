pub mod api;
pub mod config;

use api::backend::AxumBackend;
use kzgrs_backend::common::share::DaShare;
use logos_blockchain_core::mantle::{SignedMantleTx, TxHash};
use logos_blockchain_da_dispersal::{
    DispersalService,
    adapters::{
        network::libp2p::Libp2pNetworkAdapter as DispersalNetworkAdapter,
        wallet::mock::MockWalletAdapter as DispersalWalletAdapter,
    },
    backend::kzgrs::DispersalKZGRSBackend,
};
use logos_blockchain_da_network_service::backends::libp2p::executor::DaNetworkExecutorBackend;
use logos_blockchain_da_sampling::{
    backend::kzgrs::KzgrsSamplingBackend,
    storage::adapters::rocksdb::{
        RocksAdapter as SamplingStorageAdapter, converter::DaStorageConverter,
    },
};
use logos_blockchain_da_verifier::{
    backend::kzgrs::KzgrsDaVerifier,
    network::adapters::executor::Libp2pAdapter as VerifierNetworkAdapter,
    storage::adapters::rocksdb::RocksAdapter as VerifierStorageAdapter,
};
#[cfg(feature = "tracing")]
use logos_blockchain_node::Tracing;
use logos_blockchain_node::{
    BlobInfo, DaNetworkApiAdapter, NetworkBackend, LogosBlockchainDaMembership, RocksBackend, SystemSig,
    generic_services::{
        DaMembershipAdapter, DaMembershipStorageGeneric, SamplingMempoolAdapter,
        SdpMempoolAdapterGeneric, SdpService, SdpServiceAdapterGeneric, VerifierMempoolAdapter,
    },
};
use logos_blockchain_time::backends::NtpTimeBackend;
use overwatch::derive_services;
use tx_service::storage::adapters::RocksStorageAdapter;

#[cfg(feature = "tracing")]
pub(crate) type TracingService = Tracing<RuntimeServiceId>;

type DaMembershipStorage = DaMembershipStorageGeneric<RuntimeServiceId>;

pub(crate) type NetworkService = logos_blockchain_network::NetworkService<NetworkBackend, RuntimeServiceId>;

pub(crate) type BlendCoreService =
    logos_blockchain_node::generic_services::blend::BlendCoreService<DaNetworkAdapter, RuntimeServiceId>;
pub(crate) type BlendEdgeService =
    logos_blockchain_node::generic_services::blend::BlendEdgeService<DaNetworkAdapter, RuntimeServiceId>;
pub(crate) type BlendService =
    logos_blockchain_node::generic_services::blend::BlendService<DaNetworkAdapter, RuntimeServiceId>;

pub(crate) type BlockBroadcastService = broadcast_service::BlockBroadcastService<RuntimeServiceId>;

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

pub(crate) type DaVerifierService = logos_blockchain_node::generic_services::DaVerifierService<
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
    logos_blockchain_node::generic_services::DaSamplingService<DaNetworkAdapter, RuntimeServiceId>;

pub(crate) type DaNetworkService = logos_blockchain_da_network_service::NetworkService<
    DaNetworkExecutorBackend<LogosBlockchainDaMembership>,
    LogosBlockchainDaMembership,
    DaMembershipAdapter<RuntimeServiceId>,
    DaMembershipStorage,
    DaNetworkApiAdapter,
    SdpServiceAdapterGeneric<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub(crate) type MempoolService = logos_blockchain_node::generic_services::TxMempoolService<RuntimeServiceId>;

pub(crate) type DaNetworkAdapter = logos_blockchain_da_sampling::network::adapters::executor::Libp2pAdapter<
    LogosBlockchainDaMembership,
    DaMembershipAdapter<RuntimeServiceId>,
    DaMembershipStorage,
    DaNetworkApiAdapter,
    SdpServiceAdapterGeneric<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub(crate) type CryptarchiaService =
    logos_blockchain_node::generic_services::CryptarchiaService<RuntimeServiceId>;

pub(crate) type ChainNetworkService =
    logos_blockchain_node::generic_services::ChainNetworkService<DaNetworkAdapter, RuntimeServiceId>;

pub(crate) type WalletService =
    logos_blockchain_node::generic_services::WalletService<CryptarchiaService, RuntimeServiceId>;

pub(crate) type KeyManagementService =
    logos_blockchain_node::generic_services::KeyManagementService<RuntimeServiceId>;

pub(crate) type CryptarchiaLeaderService = logos_blockchain_node::generic_services::CryptarchiaLeaderService<
    CryptarchiaService,
    WalletService,
    DaNetworkAdapter,
    RuntimeServiceId,
>;

pub(crate) type TimeService = logos_blockchain_node::generic_services::TimeService<RuntimeServiceId>;

pub(crate) type ApiStorageAdapter<RuntimeServiceId> =
    logos_blockchain_api::http::storage::adapters::rocksdb::RocksAdapter<RuntimeServiceId>;

pub(crate) type ApiService = logos_blockchain_api::ApiService<
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
        kzgrs_backend::dispersal::Metadata,
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

pub(crate) type StorageService = logos_blockchain_storage::StorageService<RocksBackend, RuntimeServiceId>;

pub(crate) type SystemSigService = SystemSig<RuntimeServiceId>;

#[cfg(feature = "testing")]
type TestingApiService<RuntimeServiceId> =
    logos_blockchain_api::ApiService<api::testing::backend::TestAxumBackend, RuntimeServiceId>;

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
