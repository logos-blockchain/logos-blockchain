use futures::future::ready;
use key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
use nomos_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
use nomos_blend_proofs::quota::{
    self, VerifiedProofOfQuota,
    fixtures::{valid_proof_of_core_quota_inputs, valid_proof_of_leadership_quota_inputs},
    inputs::prove::{
        PrivateInputs, PublicInputs as PoQPublicInputs,
        private::{ProofOfCoreQuotaInputs, ProofOfLeadershipQuotaInputs},
    },
};
use nomos_core::crypto::ZkHash;

use crate::message_blend::CoreProofOfQuotaGenerator;

pub const fn poq_public_inputs_from_session_public_inputs_and_signing_key(
    (
        PoQVerificationInputsMinusSigningKey {
            core,
            leader,
            session,
        },
        signing_key,
    ): (PoQVerificationInputsMinusSigningKey, Ed25519PublicKey),
) -> PoQPublicInputs {
    PoQPublicInputs {
        signing_key: signing_key.into_inner(),
        core,
        leader,
        session,
    }
}

pub fn valid_proof_of_quota_inputs(
    core_quota: u64,
) -> (PoQVerificationInputsMinusSigningKey, ProofOfCoreQuotaInputs) {
    let (
        PoQPublicInputs {
            core,
            leader,
            session,
            ..
        },
        private_inputs,
    ) = valid_proof_of_core_quota_inputs(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
            .unwrap()
            .into_inner(),
        core_quota,
    );
    (
        PoQVerificationInputsMinusSigningKey {
            core,
            leader,
            session,
        },
        private_inputs,
    )
}

pub fn valid_proof_of_leader_inputs(
    leader_quota: u64,
) -> (
    PoQVerificationInputsMinusSigningKey,
    ProofOfLeadershipQuotaInputs,
) {
    let (
        PoQPublicInputs {
            core,
            leader,
            session,
            ..
        },
        private_inputs,
    ) = valid_proof_of_leadership_quota_inputs(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
            .unwrap()
            .into_inner(),
        leader_quota,
    );
    (
        PoQVerificationInputsMinusSigningKey {
            core,
            leader,
            session,
        },
        private_inputs,
    )
}

#[derive(Clone)]
pub struct CorePoQGeneratorFromPrivateCoreQuotaInputs(ProofOfCoreQuotaInputs);

impl CorePoQGeneratorFromPrivateCoreQuotaInputs {
    pub fn new(private_inputs: ProofOfCoreQuotaInputs) -> Self {
        Self(private_inputs)
    }
}

impl CoreProofOfQuotaGenerator for CorePoQGeneratorFromPrivateCoreQuotaInputs {
    fn generate_poq(
        &self,
        public_inputs: &PoQPublicInputs,
        key_index: u64,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync
    {
        ready(VerifiedProofOfQuota::new(
            public_inputs,
            PrivateInputs::new_proof_of_core_quota_inputs(key_index, self.0.clone()),
        ))
    }
}
