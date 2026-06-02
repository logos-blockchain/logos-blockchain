use axum::async_trait;
use lb_blend::{
    message::crypto::key_ext::Ed25519SecretKeyExt as _,
    proofs::{
        quota::{VerifiedProofOfQuota, inputs::prove::private::ProofOfLeadershipQuotaInputs},
        selection::VerifiedProofOfSelection,
    },
    scheduling::message_blend::provers::{
        BlendLayerProof, ProofsGeneratorSettings,
        core_and_leader::RealCoreAndLeaderProofsGenerator,
        leader::{LeaderProofsGenerator, RealLeaderProofsGenerator},
    },
};
use lb_blend_service::{RealProofsVerifier, core::kms::PreloadKMSBackendCorePoQGenerator};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_time_service::backends::NtpTimeBackend;
use libp2p::PeerId;

use crate::generic_services::{CryptarchiaService, SdpService, blend::pol::PolInfoProvider};

pub(crate) mod pol;

pub type BlendCoreService<RuntimeServiceId> = lb_blend_service::core::BlendService<
    lb_blend_service::core::backends::libp2p::Libp2pBlendBackend,
    PeerId,
    lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId>,
    SdpService<RuntimeServiceId>,
    RealCoreAndLeaderProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>,
    RealProofsVerifier,
    NtpTimeBackend,
    CryptarchiaService<RuntimeServiceId>,
    PolInfoProvider,
    RuntimeServiceId,
>;

#[derive(Clone)]
pub struct MockLeaderProofsGenerator;

#[async_trait]
impl LeaderProofsGenerator for MockLeaderProofsGenerator {
    fn new(
        _settings: ProofsGeneratorSettings,
        _private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        Self
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        BlendLayerProof {
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            ephemeral_signing_key: UnsecuredEd25519Key::generate_with_blake_rng(),
        }
    }
}

pub type BlendEdgeService<RuntimeServiceId> = lb_blend_service::edge::BlendService<
        lb_blend_service::edge::backends::libp2p::Libp2pBlendBackend,
        PeerId,
        <lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId> as lb_blend_service::core::network::NetworkAdapter<RuntimeServiceId>>::BroadcastSettings,
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
