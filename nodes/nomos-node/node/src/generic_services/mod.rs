use chain_leader::CryptarchiaLeader;
use chain_network::network::adapters::libp2p::LibP2pAdapter;
use chain_service::CryptarchiaConsensus;
use key_management_system_service::backend::preload::PreloadKMSBackend;
use kzgrs_backend::common::share::DaShare;
use nomos_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction, TxHash},
};
use nomos_da_network_service::{
    membership::adapters::service::MembershipServiceAdapter,
    sdp::adapters::sdp_service::SdpServiceAdapter, storage::adapters::rocksdb::RocksAdapter,
};
use nomos_da_sampling::{
    backend::kzgrs::KzgrsSamplingBackend, storage::adapters::rocksdb::converter::DaStorageConverter,
};
use nomos_da_verifier::{
    backend::kzgrs::KzgrsDaVerifier, mempool::kzgrs::KzgrsMempoolNetworkAdapter,
};
use nomos_sdp::adapters::mempool::sdp::SdpMempoolNetworkAdapter;
use nomos_storage::backends::rocksdb::RocksBackend;
use nomos_time::backends::NtpTimeBackend;
use tx_service::{backend::pool::Mempool, storage::adapters::rocksdb::RocksStorageAdapter};

use crate::{MB16, generic_services::blend::BlendService};

pub mod blend;

pub type TxMempoolService<RuntimeServiceId> = tx_service::TxMempoolService<
    tx_service::network::adapters::libp2p::Libp2pAdapter<
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
    nomos_da_sampling::mempool::sampling::SamplingMempoolNetworkAdapter<
        MempoolAdapter<RuntimeServiceId>,
        MempoolBackend<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub type TimeService<RuntimeServiceId> = nomos_time::TimeService<NtpTimeBackend, RuntimeServiceId>;

pub type VerifierMempoolAdapter<RuntimeServiceId> = KzgrsMempoolNetworkAdapter<
    tx_service::network::adapters::libp2p::Libp2pAdapter<SignedMantleTx, TxHash, RuntimeServiceId>,
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
    nomos_da_verifier::DaVerifierService<
        KzgrsDaVerifier,
        VerifierAdapter,
        nomos_da_verifier::storage::adapters::rocksdb::RocksAdapter<DaShare, DaStorageConverter>,
        MempoolAdapter,
        RuntimeServiceId,
    >;

pub type DaSamplingStorage =
    nomos_da_sampling::storage::adapters::rocksdb::RocksAdapter<DaShare, DaStorageConverter>;

pub type DaSamplingService<SamplingAdapter, RuntimeServiceId> =
    nomos_da_sampling::DaSamplingService<
        KzgrsSamplingBackend,
        SamplingAdapter,
        DaSamplingStorage,
        SamplingMempoolAdapter<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub type MempoolAdapter<RuntimeServiceId> = tx_service::network::adapters::libp2p::Libp2pAdapter<
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

pub type ChainNetworkService<SamplingAdapter, RuntimeServiceId> = chain_network::ChainNetwork<
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
    key_management_system_service::KMSService<PreloadKMSBackend, RuntimeServiceId>;

pub type WalletService<Cryptarchia, RuntimeServiceId> = nomos_wallet::WalletService<
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
        nomos_core::mantle::select::FillSize<MB16, SignedMantleTx>,
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
    tx_service::network::adapters::libp2p::Libp2pAdapter<SignedMantleTx, TxHash, RuntimeServiceId>,
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
    nomos_sdp::SdpService<SdpMempoolAdapterGeneric<RuntimeServiceId>, RuntimeServiceId>;

pub type SdpServiceAdapterGeneric<RuntimeServiceId> =
    SdpServiceAdapter<SdpMempoolAdapterGeneric<RuntimeServiceId>, RuntimeServiceId>;

pub type DaMembershipStorageGeneric<RuntimeServiceId> =
    RocksAdapter<RocksBackend, RuntimeServiceId>;
