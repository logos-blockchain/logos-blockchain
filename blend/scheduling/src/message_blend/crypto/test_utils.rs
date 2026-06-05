use core::convert::Infallible;

use async_trait::async_trait;
use futures::future::ready;
use lb_blend_message::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey, encap::ProofsVerifier,
};
use lb_blend_proofs::{
    quota::{
        self, ProofOfQuota, VerifiedProofOfQuota,
        inputs::prove::{PublicInputs, private::ProofOfLeadershipQuotaInputs},
    },
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_core::crypto::ZkHash;
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::message_blend::{
    CoreProofOfQuotaGenerator,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, core_and_leader::CoreAndLeaderProofsGenerator,
    },
};

pub struct MockCorePoQGenerator;

impl CoreProofOfQuotaGenerator for MockCorePoQGenerator {
    fn generate_poq(
        &self,
        _public_inputs: &PublicInputs,
        _key_index: u64,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync
    {
        use lb_groth16::Field as _;

        ready(Ok((
            VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
            ZkHash::ZERO,
        )))
    }
}

pub struct TestEpochChangeCoreAndLeaderProofsGenerator(pub Option<ProofOfLeadershipQuotaInputs>);

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for TestEpochChangeCoreAndLeaderProofsGenerator
{
    fn new(
        _settings: ProofsGeneratorSettings,
        _proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(None)
    }

    fn set_epoch_private(
        &mut self,
        new_epoch_private: ProofOfLeadershipQuotaInputs,
        _target_epoch: Epoch,
    ) {
        self.0 = Some(new_epoch_private);
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        None
    }
}

pub struct TestEpochChangeProofsVerifier;

#[async_trait]
impl ProofsVerifier for TestEpochChangeProofsVerifier {
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
