use lb_chain_leader_service::CryptarchiaLeader;
use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapter;
use lb_chain_service::CryptarchiaConsensus;
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction, TxHash},
};
use lb_da_network_service::{
    membership::adapters::service::MembershipServiceAdapter,
    sdp::adapters::sdp_service::SdpServiceAdapter, storage::adapters::rocksdb::RocksAdapter,
};
use lb_da_sampling_service::{
    backend::kzgrs::KzgrsSamplingBackend, storage::adapters::rocksdb::converter::DaStorageConverter,
};
use lb_da_verifier_service::{
    backend::kzgrs::KzgrsDaVerifier, mempool::kzgrs::KzgrsMempoolNetworkAdapter,
};
use lb_key_management_system_service::backend::preload::PreloadKMSBackend;
use lb_kzgrs_backend::common::share::DaShare;
use lb_sdp_service::adapters::mempool::sdp::SdpMempoolNetworkAdapter;
use lb_storage_service::backends::rocksdb::RocksBackend;
use lb_time_service::backends::NtpTimeBackend;
use lb_tx_service::{backend::pool::Mempool, storage::adapters::rocksdb::RocksStorageAdapter};

use crate::{MB16, generic_services::blend::BlendService};

pub mod blend;

pub type TxMempoolService<RuntimeServiceId> = lb_tx_service::TxMempoolService<
    lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
        SignedMantleTx,
        <SignedMantleTx as Transaction>::Hash,
        RuntimeServiceId,
    >,
    Mempool<
        HeaderId,
        SignedMantleTx,
        TxHash,
        RocksStorageAdapter<SignedMantleTx, <SignedMantleTx as Transaction>::Hash>,
        RuntimeServiceId,
    >,
    RocksStorageAdapter<SignedMantleTx, <SignedMantleTx as Transaction>::Hash>,
    RuntimeServiceId,
>;

pub type SamplingMempoolAdapter<RuntimeServiceId> =
    lb_da_sampling_service::mempool::sampling::SamplingMempoolNetworkAdapter<
        MempoolAdapter<RuntimeServiceId>,
        MempoolBackend<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub type TimeService<RuntimeServiceId> =
    lb_time_service::TimeService<NtpTimeBackend, RuntimeServiceId>;

pub type VerifierMempoolAdapter<RuntimeServiceId> = KzgrsMempoolNetworkAdapter<
    lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
        SignedMantleTx,
        TxHash,
        RuntimeServiceId,
    >,
    Mempool<
        HeaderId,
        SignedMantleTx,
        TxHash,
        RocksStorageAdapter<SignedMantleTx, <SignedMantleTx as Transaction>::Hash>,
        RuntimeServiceId,
    >,
    RuntimeServiceId,
>;

pub type DaVerifierService<VerifierAdapter, MempoolAdapter, RuntimeServiceId> =
    lb_da_verifier_service::DaVerifierService<
        KzgrsDaVerifier,
        VerifierAdapter,
        lb_da_verifier_service::storage::adapters::rocksdb::RocksAdapter<
            DaShare,
            DaStorageConverter,
        >,
        MempoolAdapter,
        RuntimeServiceId,
    >;

pub type DaSamplingStorage =
    lb_da_sampling_service::storage::adapters::rocksdb::RocksAdapter<DaShare, DaStorageConverter>;

pub type DaSamplingService<SamplingAdapter, RuntimeServiceId> =
    lb_da_sampling_service::DaSamplingService<
        KzgrsSamplingBackend,
        SamplingAdapter,
        DaSamplingStorage,
        SamplingMempoolAdapter<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub type MempoolAdapter<RuntimeServiceId> = lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
    SignedMantleTx,
    <SignedMantleTx as Transaction>::Hash,
    RuntimeServiceId,
>;

pub type MempoolBackend<RuntimeServiceId> = Mempool<
    HeaderId,
    SignedMantleTx,
    <SignedMantleTx as Transaction>::Hash,
    RocksStorageAdapter<SignedMantleTx, <SignedMantleTx as Transaction>::Hash>,
    RuntimeServiceId,
>;

pub type CryptarchiaService<RuntimeServiceId> =
    CryptarchiaConsensus<SignedMantleTx, RocksBackend, NtpTimeBackend, RuntimeServiceId>;

pub type ChainNetworkService<SamplingAdapter, RuntimeServiceId> =
    lb_chain_network_service::ChainNetwork<
        CryptarchiaService<RuntimeServiceId>,
        LibP2pAdapter<SignedMantleTx, RuntimeServiceId>,
        MempoolBackend<RuntimeServiceId>,
        MempoolAdapter<RuntimeServiceId>,
        SamplingMempoolAdapter<RuntimeServiceId>,
        KzgrsSamplingBackend,
        SamplingAdapter,
        DaSamplingStorage,
        NtpTimeBackend,
        RuntimeServiceId,
    >;

pub type KeyManagementService<RuntimeServiceId> =
    lb_key_management_system_service::KMSService<PreloadKMSBackend, RuntimeServiceId>;

pub type WalletService<Cryptarchia, RuntimeServiceId> = lb_wallet_service::WalletService<
    KeyManagementService<RuntimeServiceId>,
    Cryptarchia,
    SignedMantleTx,
    RocksBackend,
    RuntimeServiceId,
>;

pub type CryptarchiaLeaderService<Cryptarchia, Wallet, SamplingAdapter, RuntimeServiceId> =
    CryptarchiaLeader<
        BlendService<SamplingAdapter, RuntimeServiceId>,
        MempoolBackend<RuntimeServiceId>,
        MempoolAdapter<RuntimeServiceId>,
        SamplingMempoolAdapter<RuntimeServiceId>,
        lb_core::mantle::select::FillSize<MB16, SignedMantleTx>,
        KzgrsSamplingBackend,
        SamplingAdapter,
        DaSamplingStorage,
        NtpTimeBackend,
        Cryptarchia,
        Wallet,
        RuntimeServiceId,
    >;

pub type DaMembershipAdapter<RuntimeServiceId> = MembershipServiceAdapter<RuntimeServiceId>;

pub type SdpMempoolAdapterGeneric<RuntimeServiceId> = SdpMempoolNetworkAdapter<
    lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
        SignedMantleTx,
        TxHash,
        RuntimeServiceId,
    >,
    Mempool<
        HeaderId,
        SignedMantleTx,
        TxHash,
        RocksStorageAdapter<SignedMantleTx, <SignedMantleTx as Transaction>::Hash>,
        RuntimeServiceId,
    >,
    RuntimeServiceId,
>;

pub type SdpService<RuntimeServiceId> =
    lb_sdp_service::SdpService<SdpMempoolAdapterGeneric<RuntimeServiceId>, RuntimeServiceId>;

pub type SdpServiceAdapterGeneric<RuntimeServiceId> =
    SdpServiceAdapter<SdpMempoolAdapterGeneric<RuntimeServiceId>, RuntimeServiceId>;

pub type DaMembershipStorageGeneric<RuntimeServiceId> =
    RocksAdapter<RocksBackend, RuntimeServiceId>;
