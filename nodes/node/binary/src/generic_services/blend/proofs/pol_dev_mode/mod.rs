use async_trait::async_trait;
use lb_blend::{
    crypto::random_sized_bytes,
    message::crypto::{
        key_ext::Ed25519SecretKeyExt as _,
        proofs::{Error as InnerVerifierError, PoQVerificationInputsMinusSigningKey},
    },
    proofs::{
        quota::{
            ProofOfQuota, VerifiedProofOfQuota,
            inputs::prove::{private::ProofOfLeadershipQuotaInputs, public::LeaderInputs},
        },
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    },
    scheduling::message_blend::{
        CoreProofOfQuotaGenerator,
        provers::{
            BlendLayerProof, ProofsGeneratorSettings,
            core::{CoreProofsGenerator as _, RealCoreProofsGenerator},
            core_and_leader::CoreAndLeaderProofsGenerator,
            leader::LeaderProofsGenerator,
        },
    },
};
use lb_blend_service::{ProofsVerifier as ProofsVerifierTrait, RealProofsVerifier};
use lb_core::{codec::DeserializeOp as _, crypto::ZkHash};
use lb_groth16::{Field as _, fr_to_bytes};
use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use lb_poq::PoQProof;

#[cfg(test)]
mod tests;

const DUMMY_POQ_ZK_NULLIFIER: ZkHash = ZkHash::ZERO;
const LOG_TARGET: &str = "node::blend::proofs";

pub struct MockedCoreProofsGenerator<CorePoQGenerator>(RealCoreProofsGenerator<CorePoQGenerator>);

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for MockedCoreProofsGenerator<CorePoQGenerator>
where
    CorePoQGenerator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    fn new(
        settings: ProofsGeneratorSettings,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(RealCoreProofsGenerator::new(
            settings,
            core_proof_of_quota_generator,
        ))
    }

    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs) {
        self.0.rotate_epoch(new_epoch_public);
    }

    fn set_epoch_private(&mut self, _new_epoch_private: ProofOfLeadershipQuotaInputs) {
        tracing::trace!(target: LOG_TARGET, "Core proof generator still generates mocked leadership PoQ proofs, so epoch private info won't have any effects.");
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        tracing::debug!(target: LOG_TARGET, "Core PoQ proof requested.");
        self.0.get_next_proof().await
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        tracing::debug!(target: LOG_TARGET, "Leadership PoQ proof requested. A mock one will be returned for now.");
        Some(random_proof())
    }
}

pub struct MockedEdgeProofsGenerator;

#[async_trait]
impl LeaderProofsGenerator for MockedEdgeProofsGenerator {
    fn new(
        _settings: ProofsGeneratorSettings,
        _private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        Self
    }

    fn rotate_epoch(
        &mut self,
        _new_epoch_public: LeaderInputs,
        _new_private_inputs: ProofOfLeadershipQuotaInputs,
    ) {
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        random_proof()
    }
}

// Randomly generates PoQ and PoSel from bytes until a valid combination of both
// is generated.
fn random_proof() -> BlendLayerProof {
    loop {
        let proof_random_bytes = random_sized_bytes::<{ size_of::<PoQProof>() }>();
        let poq_bytes: Vec<_> = fr_to_bytes(&DUMMY_POQ_ZK_NULLIFIER)
            .into_iter()
            .chain(proof_random_bytes)
            .collect();
        let Ok(proof_of_quota) = VerifiedProofOfQuota::from_bytes(&poq_bytes[..]) else {
            continue;
        };
        let Ok(proof_of_selection) = VerifiedProofOfSelection::from_bytes(
            &random_sized_bytes::<{ size_of::<ProofOfSelection>() }>()[..],
        ) else {
            continue;
        };
        return BlendLayerProof {
            ephemeral_signing_key: UnsecuredEd25519Key::generate_with_blake_rng(),
            proof_of_quota,
            proof_of_selection,
        };
    }
}

#[derive(Clone)]
pub struct MockedBlendProofsVerifier(RealProofsVerifier);

impl ProofsVerifierTrait for MockedBlendProofsVerifier {
    type Error = InnerVerifierError;

    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self(RealProofsVerifier::new(public_inputs))
    }

    fn start_epoch_transition(&mut self, new_pol_inputs: LeaderInputs) {
        self.0.start_epoch_transition(new_pol_inputs);
    }

    fn complete_epoch_transition(&mut self) {
        self.0.complete_epoch_transition();
    }

    #[expect(clippy::cognitive_complexity, reason = "Tracing macros.")]
    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        let key_nullifier = proof.key_nullifier();
        tracing::debug!(target: LOG_TARGET, "Verifying PoQ with key nullifier: {key_nullifier:?}");
        if proof.key_nullifier() == DUMMY_POQ_ZK_NULLIFIER {
            tracing::debug!(target: LOG_TARGET, "Mocked PoL PoQ proof received (automatically verified successfully).");
            Ok(VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
        } else {
            tracing::debug!(target: LOG_TARGET, "Core PoQ proof received.");
            let verification_result = self.0.verify_proof_of_quota(proof, signing_key).inspect_err(|e| {
                tracing::debug!(target: LOG_TARGET, "Core PoQ proof with key nullifier {key_nullifier:?} verification failed with error {e:?}");
            })?;
            tracing::debug!(target: LOG_TARGET, "Core PoQ proof with key nullifier {key_nullifier:?} verified successfully.");
            Ok(verification_result)
        }
    }

    #[expect(clippy::cognitive_complexity, reason = "Tracing macros.")]
    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        let key_nullifier = inputs.key_nullifier;
        tracing::debug!(target: LOG_TARGET, "Verifying PoSel for key nullifier: {key_nullifier:?}");
        if inputs.key_nullifier == DUMMY_POQ_ZK_NULLIFIER {
            tracing::debug!(target: LOG_TARGET, "Mocked PoL PoSel proof received (automatically verified successfully).");
            Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
                proof,
            ))
        } else {
            tracing::debug!(target: LOG_TARGET, "Core PoSel proof received.");
            let verified_proof_of_selection = self.0.verify_proof_of_selection(proof, inputs).inspect_err(|e| {
                tracing::debug!(target: LOG_TARGET, "Core PoSel proof for key nullifier {key_nullifier:?} verification failed with error {e:?}");
            })?;
            tracing::debug!(target: LOG_TARGET, "Core PoSel proof for key nullifier {key_nullifier:?} verified successfully.");
            Ok(verified_proof_of_selection)
        }
    }
}
