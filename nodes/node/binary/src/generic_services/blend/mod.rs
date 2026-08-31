use core::{
    fmt::{Debug, Display},
    marker::PhantomData,
};

use axum::async_trait;
use lb_blend::{
    message::crypto::key_ext::Ed25519SecretKeyExt as _,
    proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection},
    scheduling::message_blend::provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        core_leader_and_pow::RealCoreLeaderAndPowProofsGenerator, leader::LeaderProofsGenerator,
        leader_and_pow::RealLeaderAndPowProofsGenerator,
    },
};
use lb_blend_service::{
    Components, RealProofsVerifier,
    core::{kms::PreloadKMSBackendCorePoQGenerator, service_components::Components},
    edge::service_components::Components,
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_storage_service::{
    StorageService, backends::rocksdb::RocksBackend, recovery::StorageRecoveryBackend,
};
use lb_time_service::backends::NtpTimeBackend;
use lb_utils::blake_rng::BlakeRng;
use libp2p::PeerId;
use overwatch::services::AsServiceId;

use crate::generic_services::{
    CryptarchiaService, MempoolNetworkAdapter, MempoolPool, SdpService, blend::pol::PolInfoProvider,
};

pub(crate) mod pol;

/// Blend's exit door on this node: block proposals go back onto the chain's
/// gossipsub topic, transactions go to the mempool.
pub type BlendPayloadDispatcher<RuntimeServiceId> =
    lb_blend_service::core::dispatcher::libp2p::Libp2pPayloadDispatcher<
        MempoolNetworkAdapter<RuntimeServiceId>,
        MempoolPool<RuntimeServiceId>,
        RuntimeServiceId,
    >;

/// Everything the node configures Blend with, across all three modes.
pub type BlendSettings<RuntimeServiceId> = lb_blend_service::settings::Settings<
    lb_blend_service::core::backends::libp2p::Libp2pBlendBackendSettings,
    lb_blend_service::edge::backends::libp2p::Libp2pBlendBackendSettings,
    BlendBroadcastSettings<RuntimeServiceId>,
>;

pub type BlendRecoveryBackend<RuntimeServiceId> = StorageRecoveryBackend<
    lb_blend_service::core::CoreServiceState<BlendSettings<RuntimeServiceId>>,
    BlendSettings<RuntimeServiceId>,
    RocksBackend,
    RuntimeServiceId,
>;

#[derive(Clone)]
pub struct MockLeaderProofsGenerator;

#[async_trait]
impl LeaderProofsGenerator for MockLeaderProofsGenerator {
    fn new(
        _settings: ProofsGeneratorSettings,
        _winning_pol_info_stream: WinningPolInfoStream,
    ) -> Self {
        Self
    }

    async fn get_next_proof(&mut self) -> Option<BlendLayerProof> {
        Some(BlendLayerProof {
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            ephemeral_signing_key: UnsecuredEd25519Key::generate_with_blake_rng(),
        })
    }
}

/// This node's answer to "which collaborators does Blend run on?", stated once.
///
/// The three service aliases above described the *old* shape, where a proxy
/// started and stopped one Overwatch service per mode. Blend is one service
/// now, so what it needs is the leaf types, named together: the common ones
/// through [`Components`], and each mode's own through its bundle.
pub struct BlendComponents<RuntimeServiceId>(PhantomData<fn() -> RuntimeServiceId>);

impl<RuntimeServiceId> Components<RuntimeServiceId> for BlendComponents<RuntimeServiceId>
where
    RuntimeServiceId: Debug
        + Display
        + Clone
        + Send
        + Sync
        + 'static
        + AsServiceId<StorageService<RocksBackend, RuntimeServiceId>>,
{
    type Settings = BlendSettings<RuntimeServiceId>;
    type NodeId = PeerId;
    type Dispatcher = BlendPayloadDispatcher<RuntimeServiceId>;
    type TimeBackend = NtpTimeBackend;
    type ChainService = CryptarchiaService<RuntimeServiceId>;
    type PolInfoProvider = PolInfoProvider;
    type SdpService = SdpService<RuntimeServiceId>;
    type StateStorage = BlendRecoveryBackend<RuntimeServiceId>;
}

impl<RuntimeServiceId> Components<RuntimeServiceId> for BlendComponents<RuntimeServiceId>
where
    RuntimeServiceId: Debug
        + Display
        + Clone
        + Send
        + Sync
        + 'static
        + AsServiceId<StorageService<RocksBackend, RuntimeServiceId>>,
{
    type Rng = BlakeRng;
    type ProofsVerifier = RealProofsVerifier;
    type CorePoQGenerator = PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>;
    type Backend = lb_blend_service::core::backends::libp2p::Libp2pBlendBackend<RealProofsVerifier>;
    type ProofsGenerator =
        RealCoreLeaderAndPowProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>;
}

impl<RuntimeServiceId> Components<RuntimeServiceId> for BlendComponents<RuntimeServiceId>
where
    RuntimeServiceId: Debug
        + Display
        + Clone
        + Send
        + Sync
        + 'static
        + AsServiceId<StorageService<RocksBackend, RuntimeServiceId>>,
{
    type Backend = lb_blend_service::edge::backends::libp2p::Libp2pBlendBackend;
    // The edge and core proofs generators are pinned to the same verification
    // logic here, which is what the old two-service wiring could only ask for
    // by convention.
    type ProofsGenerator = RealLeaderAndPowProofsGenerator;
}

pub type BlendService<RuntimeServiceId> =
    lb_blend_service::BlendService<BlendComponents<RuntimeServiceId>, RuntimeServiceId>;

pub type BlendBroadcastSettings<RuntimeServiceId> = <BlendPayloadDispatcher<RuntimeServiceId> as lb_blend_service::core::dispatcher::PayloadDispatcher<
    RuntimeServiceId,
>>::Settings;
