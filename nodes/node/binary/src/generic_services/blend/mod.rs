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
use lb_blend_service::{RealProofsVerifier, core::kms::PreloadKMSBackendCorePoQGenerator};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_storage_service::{backends::rocksdb::RocksBackend, recovery::StorageRecoveryBackend};
use lb_time_service::backends::NtpTimeBackend;
use libp2p::PeerId;

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

pub type BlendCoreRecoveryBackend<RuntimeServiceId> = StorageRecoveryBackend<
    lb_blend_service::core::CoreServiceState<
        lb_blend_service::core::backends::libp2p::Libp2pBlendBackendSettings,
        BlendBroadcastSettings<RuntimeServiceId>,
    >,
    lb_blend_service::core::settings::StartingBlendConfig<
        lb_blend_service::core::backends::libp2p::Libp2pBlendBackendSettings,
        BlendBroadcastSettings<RuntimeServiceId>,
    >,
    RocksBackend,
    RuntimeServiceId,
>;

pub type BlendCoreService<RuntimeServiceId> = lb_blend_service::core::BlendService<
    lb_blend_service::core::backends::libp2p::Libp2pBlendBackend<RealProofsVerifier>,
    PeerId,
    BlendPayloadDispatcher<RuntimeServiceId>,
    SdpService<RuntimeServiceId>,
    RealCoreLeaderAndPowProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>,
    RealProofsVerifier,
    NtpTimeBackend,
    CryptarchiaService<RuntimeServiceId>,
    PolInfoProvider,
    BlendCoreRecoveryBackend<RuntimeServiceId>,
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

pub type BlendEdgeService<RuntimeServiceId> = lb_blend_service::edge::BlendService<
    lb_blend_service::edge::backends::libp2p::Libp2pBlendBackend,
    PeerId,
    RealLeaderAndPowProofsGenerator,
    NtpTimeBackend,
    CryptarchiaService<RuntimeServiceId>,
    PolInfoProvider,
    RuntimeServiceId,
>;
pub type BlendService<RuntimeServiceId> = lb_blend_service::BlendService<
    BlendCoreService<RuntimeServiceId>,
    BlendEdgeService<RuntimeServiceId>,
    SdpService<RuntimeServiceId>,
    RuntimeServiceId,
>;

pub type BlendBroadcastSettings<RuntimeServiceId> = <BlendPayloadDispatcher<RuntimeServiceId> as lb_blend_service::core::dispatcher::PayloadDispatcher<
    RuntimeServiceId,
>>::Settings;
