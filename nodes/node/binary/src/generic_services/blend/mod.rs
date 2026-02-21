use core::{
    fmt::{Debug, Display},
    future::ready,
};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use lb_blend::{
    proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs,
    scheduling::message_blend::provers::{
        core_and_leader::RealCoreAndLeaderProofsGenerator, leader::RealLeaderProofsGenerator,
    },
};
use lb_blend_service::{
    RealProofsVerifier,
    core::kms::PreloadKMSBackendCorePoQGenerator,
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    membership::service::Adapter,
};
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_chain_leader_service::LeaderMsg;
use lb_core::crypto::ZkHash;
use lb_libp2p::PeerId;
use lb_pol::{PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};
use lb_poq::AGED_NOTE_MERKLE_TREE_HEIGHT;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::backends::NtpTimeBackend;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot::channel;
use tokio_stream::wrappers::WatchStream;

use crate::generic_services::{
    ChainNetworkService, CryptarchiaLeaderService, CryptarchiaService, SdpService, WalletService,
};

pub(crate) mod pol;

pub type BlendMembershipAdapter<RuntimeServiceId> =
    Adapter<BlockBroadcastService<RuntimeServiceId>, PeerId>;
pub type BlendCoreService<RuntimeServiceId> = lb_blend_service::core::BlendService<
    lb_blend_service::core::backends::libp2p::Libp2pBlendBackend,
    PeerId,
    lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId>,
    BlendMembershipAdapter<RuntimeServiceId>,
    SdpService<RuntimeServiceId>,
    RealCoreAndLeaderProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>,
    RealProofsVerifier,
    NtpTimeBackend,
    CryptarchiaService<RuntimeServiceId>,
    PolInfoProvider,
    RuntimeServiceId,
>;
pub type BlendEdgeService<RuntimeServiceId> = lb_blend_service::edge::BlendService<
        lb_blend_service::edge::backends::libp2p::Libp2pBlendBackend,
        PeerId,
        <lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId> as lb_blend_service::core::network::NetworkAdapter<RuntimeServiceId>>::BroadcastSettings,
        BlendMembershipAdapter<RuntimeServiceId>,
        RealLeaderProofsGenerator,
        NtpTimeBackend,
        CryptarchiaService<RuntimeServiceId>,
        PolInfoProvider,
        RuntimeServiceId
    >;
pub type BlendService<RuntimeServiceId> = lb_blend_service::BlendService<
    BlendCoreService<RuntimeServiceId>,
    BlendEdgeService<RuntimeServiceId>,
    RuntimeServiceId,
>;
