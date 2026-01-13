use core::convert::Infallible;

use async_trait::async_trait;
use futures::future::ready;
use key_management_system_keys::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use nomos_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey, encap::ProofsVerifier,
};
use nomos_blend_proofs::{
    quota::{
        self, ProofOfQuota, VerifiedProofOfQuota,
        inputs::prove::{
            PublicInputs, private::ProofOfLeadershipQuotaInputs, public::LeaderInputs,
        },
    },
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use nomos_core::crypto::ZkHash;

use crate::message_blend::{
    CoreProofOfQuotaGenerator,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, core_and_leader::CoreAndLeaderProofsGenerator,
        leader::LeaderProofsGenerator,
    },
};

pub struct TestEpochChangeLeaderProofsGenerator(
    pub ProofsGeneratorSettings,
    pub ProofOfLeadershipQuotaInputs,
);

#[async_trait]
impl LeaderProofsGenerator for TestEpochChangeLeaderProofsGenerator {
    fn new(
        settings: ProofsGeneratorSettings,
        private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        Self(settings, private_inputs)
    }

    fn rotate_epoch(
        &mut self,
        new_epoch_public: LeaderInputs,
        new_private_inputs: ProofOfLeadershipQuotaInputs,
    ) {
        self.0.public_inputs.leader = new_epoch_public;
        self.1 = new_private_inputs;
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        BlendLayerProof {
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            ephemeral_signing_key: UnsecuredEd25519Key::from_bytes(&[0; _]),
        }
    }
}

pub struct MockCorePoQGenerator;

impl CoreProofOfQuotaGenerator for MockCorePoQGenerator {
    fn generate_poq(
        &self,
        _public_inputs: &PublicInputs,
        _key_index: u64,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync
    {
        use groth16::Field as _;

        ready(Ok((
            VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
            ZkHash::ZERO,
        )))
    }
}

pub struct TestEpochChangeCoreAndLeaderProofsGenerator(
    pub ProofsGeneratorSettings,
    pub Option<ProofOfLeadershipQuotaInputs>,
);

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for TestEpochChangeCoreAndLeaderProofsGenerator
{
    fn new(settings: ProofsGeneratorSettings, _proof_of_quota_generator: CorePoQGenerator) -> Self {
        Self(settings, None)
    }

    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs) {
        self.0.public_inputs.leader = new_epoch_public;
    }

    fn set_epoch_private(&mut self, new_epoch_private: ProofOfLeadershipQuotaInputs) {
        self.1 = Some(new_epoch_private);
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }
}

pub struct TestEpochChangeProofsVerifier(
    pub PoQVerificationInputsMinusSigningKey,
    pub Option<LeaderInputs>,
);

#[async_trait]
impl ProofsVerifier for TestEpochChangeProofsVerifier {
    type Error = Infallible;

    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self(public_inputs, None)
    }

    fn start_epoch_transition(&mut self, new_pol_inputs: LeaderInputs) {
        self.1 = Some(self.0.leader);
        self.0.leader = new_pol_inputs;
    }

    fn complete_epoch_transition(&mut self) {
        self.1 = None;
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
