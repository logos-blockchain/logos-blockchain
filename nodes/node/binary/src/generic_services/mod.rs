use logos_blockchain_chain_leader_service::CryptarchiaLeader;
use logos_blockchain_chain_network_service::network::adapters::libp2p::LibP2pAdapter;
use logos_blockchain_chain_service::CryptarchiaConsensus;
use logos_blockchain_key_management_system_service::backend::preload::PreloadKMSBackend;
use logos_blockchain_kzgrs_backend::common::share::DaShare;
use logos_blockchain_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction, TxHash},
};
use logos_blockchain_da_network_service::{
    membership::adapters::service::MembershipServiceAdapter,
    sdp::adapters::sdp_service::SdpServiceAdapter, storage::adapters::rocksdb::RocksAdapter,
};
use logos_blockchain_da_sampling_service::{
    backend::kzgrs::KzgrsSamplingBackend, storage::adapters::rocksdb::converter::DaStorageConverter,
};
use logos_blockchain_da_verifier_service::{
    backend::kzgrs::KzgrsDaVerifier, mempool::kzgrs::KzgrsMempoolNetworkAdapter,
};
use logos_blockchain_sdp_service::adapters::mempool::sdp::SdpMempoolNetworkAdapter;
use logos_blockchain_storage_service::backends::rocksdb::RocksBackend;
use logos_blockchain_time_service::backends::NtpTimeBackend;
use logos_blockchain_tx_service::{backend::pool::Mempool, storage::adapters::rocksdb::RocksStorageAdapter};

use crate::{MB16, generic_services::blend::BlendService};

pub mod blend;

pub type TxMempoolService<RuntimeServiceId> = logos_blockchain_tx_service::TxMempoolService<
    logos_blockchain_tx_service::network::adapters::libp2p::Libp2pAdapter<
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
    logos_blockchain_da_sampling_service::mempool::sampling::SamplingMempoolNetworkAdapter<
        MempoolAdapter<RuntimeServiceId>,
        MempoolBackend<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub type TimeService<RuntimeServiceId> = logos_blockchain_time_service::TimeService<NtpTimeBackend, RuntimeServiceId>;

pub type VerifierMempoolAdapter<RuntimeServiceId> = KzgrsMempoolNetworkAdapter<
    logos_blockchain_tx_service::network::adapters::libp2p::Libp2pAdapter<SignedMantleTx, TxHash, RuntimeServiceId>,
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
    logos_blockchain_da_verifier_service::DaVerifierService<
        KzgrsDaVerifier,
        VerifierAdapter,
        logos_blockchain_da_verifier_service::storage::adapters::rocksdb::RocksAdapter<DaShare, DaStorageConverter>,
        MempoolAdapter,
        RuntimeServiceId,
    >;

pub type DaSamplingStorage =
    logos_blockchain_da_sampling_service::storage::adapters::rocksdb::RocksAdapter<DaShare, DaStorageConverter>;

pub type DaSamplingService<SamplingAdapter, RuntimeServiceId> =
    logos_blockchain_da_sampling_service::DaSamplingService<
        KzgrsSamplingBackend,
        SamplingAdapter,
        DaSamplingStorage,
        SamplingMempoolAdapter<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub type MempoolAdapter<RuntimeServiceId> = logos_blockchain_tx_service::network::adapters::libp2p::Libp2pAdapter<
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
    CryptarchiaConsensus<SignedMantleTx, RocksBackend, RuntimeServiceId>;

pub type ChainNetworkService<SamplingAdapter, RuntimeServiceId> = logos_blockchain_chain_network_service::ChainNetwork<
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
    logos_blockchain_key_management_system_service::KMSService<PreloadKMSBackend, RuntimeServiceId>;

pub type WalletService<Cryptarchia, RuntimeServiceId> = logos_blockchain_wallet_service::WalletService<
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
        logos_blockchain_core::mantle::select::FillSize<MB16, SignedMantleTx>,
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
    logos_blockchain_tx_service::network::adapters::libp2p::Libp2pAdapter<SignedMantleTx, TxHash, RuntimeServiceId>,
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
    logos_blockchain_sdp_service::SdpService<SdpMempoolAdapterGeneric<RuntimeServiceId>, RuntimeServiceId>;

pub type SdpServiceAdapterGeneric<RuntimeServiceId> =
    SdpServiceAdapter<SdpMempoolAdapterGeneric<RuntimeServiceId>, RuntimeServiceId>;

pub type DaMembershipStorageGeneric<RuntimeServiceId> =
    RocksAdapter<RocksBackend, RuntimeServiceId>;
