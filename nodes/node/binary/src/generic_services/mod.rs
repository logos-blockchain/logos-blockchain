use lb_chain_leader_service::CryptarchiaLeader;
use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapter;
use lb_chain_service::CryptarchiaConsensus;
use lb_core::{
    header::HeaderId,
    mantle::{
        SignedMantleTx,
        traits::Hashable,
        transactions::{hash::TxHash, states::Preverified},
    },
};
use lb_key_management_system_service::backend::preload::PreloadKMSBackend;
use lb_sdp_service::{SdpSettings, state::SdpState};
use lb_storage_service::{backends::rocksdb::RocksBackend, recovery::StorageRecoveryBackend};
use lb_time_service::backends::NtpTimeBackend;
use lb_tx_service::{backend::pool::Mempool, storage::adapters::rocksdb::RocksStorageAdapter};

use crate::generic_services::blend::BlendService;

pub mod blend;
pub mod sdp;

pub type MempoolNetworkAdapter<RuntimeServiceId> =
    lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
        SignedMantleTx<Preverified>,
        <SignedMantleTx<Preverified> as Hashable>::Hash,
        RuntimeServiceId,
    >;

pub type MempoolRocksStorageAdapter = RocksStorageAdapter<
    SignedMantleTx<Preverified>,
    <SignedMantleTx<Preverified> as Hashable>::Hash,
>;

pub type MempoolPool<RuntimeServiceId> = Mempool<
    HeaderId,
    SignedMantleTx<Preverified>,
    TxHash,
    MempoolRocksStorageAdapter,
    RuntimeServiceId,
>;

pub type TxMempoolService<RuntimeServiceId> = lb_tx_service::TxMempoolService<
    MempoolNetworkAdapter<RuntimeServiceId>,
    MempoolPool<RuntimeServiceId>,
    MempoolRocksStorageAdapter,
    RuntimeServiceId,
>;

pub type TimeService<RuntimeServiceId> =
    lb_time_service::TimeService<NtpTimeBackend, RuntimeServiceId>;

pub type MempoolAdapter<RuntimeServiceId> = lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
    SignedMantleTx<Preverified>,
    <SignedMantleTx<Preverified> as Hashable>::Hash,
    RuntimeServiceId,
>;

pub type MempoolBackend<RuntimeServiceId> = Mempool<
    HeaderId,
    SignedMantleTx<Preverified>,
    <SignedMantleTx<Preverified> as Hashable>::Hash,
    RocksStorageAdapter<
        SignedMantleTx<Preverified>,
        <SignedMantleTx<Preverified> as Hashable>::Hash,
    >,
    RuntimeServiceId,
>;

pub type CryptarchiaService<RuntimeServiceId> = CryptarchiaConsensus<
    SignedMantleTx<Preverified>,
    RocksBackend,
    NtpTimeBackend,
    RuntimeServiceId,
>;

pub type ChainNetworkService<RuntimeServiceId> = lb_chain_network_service::ChainNetwork<
    CryptarchiaService<RuntimeServiceId>,
    LibP2pAdapter<SignedMantleTx<Preverified>, RuntimeServiceId>,
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
    SignedMantleTx<Preverified>,
    RocksBackend,
    RuntimeServiceId,
>;

pub type CryptarchiaLeaderService<Cryptarchia, ChainNetwork, Wallet, RuntimeServiceId> =
    CryptarchiaLeader<
        BlendService<RuntimeServiceId>,
        MempoolBackend<RuntimeServiceId>,
        MempoolAdapter<RuntimeServiceId>,
        NtpTimeBackend,
        Cryptarchia,
        ChainNetwork,
        Wallet,
        RuntimeServiceId,
    >;

pub type SdpMempoolAdapter<RuntimeServiceId> = sdp::mempool::SdpMempoolAdapter<
    lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
        SignedMantleTx<Preverified>,
        TxHash,
        RuntimeServiceId,
    >,
    Mempool<
        HeaderId,
        SignedMantleTx<Preverified>,
        TxHash,
        RocksStorageAdapter<
            SignedMantleTx<Preverified>,
            <SignedMantleTx<Preverified> as Hashable>::Hash,
        >,
        RuntimeServiceId,
    >,
    RuntimeServiceId,
>;

pub type SdpWalletAdapter<RuntimeServiceId> = sdp::wallet::SdpWalletAdapter<
    WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
    RuntimeServiceId,
>;

pub type SdpRecoveryBackend<RuntimeServiceId> =
    StorageRecoveryBackend<SdpState, SdpSettings, RocksBackend, RuntimeServiceId>;

pub type SdpService<RuntimeServiceId> = lb_sdp_service::SdpService<
    SdpMempoolAdapter<RuntimeServiceId>,
    SdpWalletAdapter<RuntimeServiceId>,
    CryptarchiaService<RuntimeServiceId>,
    SdpRecoveryBackend<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub type PoWService<RuntimeServiceId> = lb_pow_service::PoWService<
    CryptarchiaService<RuntimeServiceId>,
    BlendService<RuntimeServiceId>,
    WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
    RocksBackend,
    RuntimeServiceId,
>;
