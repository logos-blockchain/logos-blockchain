use lb_blend::scheduling::message_blend::provers::{
    core_and_leader::RealCoreAndLeaderProofsGenerator, leader::RealLeaderProofsGenerator,
};
use lb_blend_service::{
    RealProofsVerifier, core::kms::PreloadKMSBackendCorePoQGenerator, membership::service::Adapter,
};
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_time_service::backends::NtpTimeBackend;
use libp2p::PeerId;

use crate::generic_services::{CryptarchiaService, SdpService, blend::pol::PolInfoProvider};

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

pub type BlendBroadcastSettings<RuntimeServiceId> =
    <lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId> as lb_blend_service::core::network::NetworkAdapter<RuntimeServiceId>>::BroadcastSettings;
