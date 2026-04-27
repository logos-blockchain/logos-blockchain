use lb_chain_leader_service::CryptarchiaLeader;
use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapter;
use lb_chain_service::CryptarchiaConsensus;
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction, TxHash},
};
use lb_key_management_system_service::backend::preload::PreloadKMSBackend;
use lb_storage_service::backends::rocksdb::RocksBackend;
use lb_time_service::backends::NtpTimeBackend;
use lb_tx_service::{backend::pool::Mempool, storage::adapters::rocksdb::RocksStorageAdapter};

use crate::{MB16, generic_services::blend::BlendService};

pub mod blend;
pub mod sdp;

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

pub type TimeService<RuntimeServiceId> =
    lb_time_service::TimeService<NtpTimeBackend, RuntimeServiceId>;

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

pub type ChainNetworkService<RuntimeServiceId> = lb_chain_network_service::ChainNetwork<
    CryptarchiaService<RuntimeServiceId>,
    LibP2pAdapter<SignedMantleTx, RuntimeServiceId>,
    MempoolBackend<RuntimeServiceId>,
    MempoolAdapter<RuntimeServiceId>,
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

pub type CryptarchiaLeaderService<Cryptarchia, ChainNetwork, Wallet, RuntimeServiceId> =
    CryptarchiaLeader<
        BlendService<RuntimeServiceId>,
        MempoolBackend<RuntimeServiceId>,
        MempoolAdapter<RuntimeServiceId>,
        lb_core::mantle::select::FillSize<MB16, SignedMantleTx>,
        NtpTimeBackend,
        Cryptarchia,
        ChainNetwork,
        Wallet,
        RuntimeServiceId,
    >;

pub type SdpMempoolAdapter<RuntimeServiceId> = sdp::mempool::SdpMempoolAdapter<
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

pub type SdpWalletAdapter<RuntimeServiceId> = sdp::wallet::SdpWalletAdapter<
    WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
    RuntimeServiceId,
>;

pub type SdpService<RuntimeServiceId> = lb_sdp_service::SdpService<
    SdpMempoolAdapter<RuntimeServiceId>,
    SdpWalletAdapter<RuntimeServiceId>,
    CryptarchiaService<RuntimeServiceId>,
    RuntimeServiceId,
>;
