use core::convert::Infallible;

use axum::async_trait;
use lb_blend::{
    message::crypto::{
        key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
    },
    proofs::{
        quota::{
            ProofOfQuota, VerifiedProofOfQuota,
            inputs::prove::{private::ProofOfLeadershipQuotaInputs, public::LeaderInputs},
        },
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    },
    scheduling::message_blend::provers::{
        BlendLayerProof, ProofsGeneratorSettings, core_and_leader::CoreAndLeaderProofsGenerator,
        leader::LeaderProofsGenerator,
    },
};
use lb_blend_service::ProofsVerifier;
use lb_chain_service::Epoch;
use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use lb_time_service::backends::NtpTimeBackend;
use libp2p::PeerId;

use crate::generic_services::{CryptarchiaService, SdpService, blend::pol::PolInfoProvider};

pub(crate) mod pol;

#[derive(Clone)]
pub struct MockCoreAndLeaderProofsGenerator;

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for MockCoreAndLeaderProofsGenerator
{
    fn new(
        _settings: ProofsGeneratorSettings,
        _core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self
    }

    fn set_epoch_private(
        &mut self,
        _new_epoch_private: ProofOfLeadershipQuotaInputs,
        _new_epoch_public: LeaderInputs,
        _new_epoch: Epoch,
    ) {
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        Some(mock_blend_proof())
    }
}

fn mock_blend_proof() -> BlendLayerProof {
    BlendLayerProof {
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
        proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
        ephemeral_signing_key: UnsecuredEd25519Key::generate_with_blake_rng(),
    }
}

#[derive(Clone)]
pub struct MockProofsVerifier;

impl ProofsVerifier for MockProofsVerifier {
    type Error = Infallible;

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        Ok(VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        _inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
            proof,
        ))
    }
}

pub type BlendCoreService<RuntimeServiceId> = lb_blend_service::core::BlendService<
    lb_blend_service::core::backends::libp2p::Libp2pBlendBackend,
    PeerId,
    lb_blend_service::core::network::libp2p::Libp2pAdapter<RuntimeServiceId>,
    SdpService<RuntimeServiceId>,
    // TODO: Re-establish real proof generator once session removal is complete.
    // RealCoreAndLeaderProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>,
    MockCoreAndLeaderProofsGenerator,
    // TODO: Re-establish real proof verifier once session removal is complete.
    // RealProofsVerifier,
    MockProofsVerifier,
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
        // TODO: Re-establish real proof generator once session removal is complete.
        // RealLeaderProofsGenerator,
        MockLeaderProofsGenerator,
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
