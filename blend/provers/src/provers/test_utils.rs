use futures::future::ready;
use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
use lb_blend_proofs::quota::{
    self, KeyIndex, Quota, VerifiedProofOfQuota,
    fixtures::{
        valid_proof_of_core_quota_inputs, valid_proof_of_leadership_quota_inputs,
        valid_proof_of_work_quota_inputs,
    },
    inputs::prove::{
        PrivateInputs, PublicInputs as PoQPublicInputs,
        private::{ProofOfCoreQuotaInputs, ProofOfLeadershipQuotaInputs},
    },
};
use lb_core::crypto::ZkHash;
use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};

use crate::CoreProofOfQuotaGenerator;

pub const fn poq_public_inputs_from_epoch_public_inputs_and_signing_key(
    (PoQVerificationInputsMinusSigningKey { core, leader, pow }, signing_key): (
        PoQVerificationInputsMinusSigningKey,
        Ed25519PublicKey,
    ),
) -> PoQPublicInputs {
    PoQPublicInputs {
        signing_key: signing_key.into_inner(),
        core,
        leader,
        pow,
    }
}

pub fn valid_proof_of_quota_inputs(
    core_quota: Quota,
) -> (PoQVerificationInputsMinusSigningKey, ProofOfCoreQuotaInputs) {
    let (
        PoQPublicInputs {
            core, leader, pow, ..
        },
        private_inputs,
    ) = valid_proof_of_core_quota_inputs(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
            .unwrap()
            .into_inner(),
        core_quota,
    );
    (
        PoQVerificationInputsMinusSigningKey { core, leader, pow },
        private_inputs,
    )
}

pub fn valid_proof_of_leader_inputs(
    leader_quota: Quota,
) -> (
    PoQVerificationInputsMinusSigningKey,
    ProofOfLeadershipQuotaInputs,
) {
    let (
        PoQPublicInputs {
            core, leader, pow, ..
        },
        private_inputs,
    ) = valid_proof_of_leadership_quota_inputs(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
            .unwrap()
            .into_inner(),
        leader_quota,
    );
    (
        PoQVerificationInputsMinusSigningKey { core, leader, pow },
        private_inputs,
    )
}

pub fn valid_proof_of_work_inputs(pow_quota: Quota) -> PoQVerificationInputsMinusSigningKey {
    let (
        PoQPublicInputs {
            core, leader, pow, ..
        },
        _,
    ) = valid_proof_of_work_quota_inputs(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
            .unwrap()
            .into_inner(),
        pow_quota,
    );
    PoQVerificationInputsMinusSigningKey { core, leader, pow }
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
        key_index: KeyIndex,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync
    {
        ready(VerifiedProofOfQuota::new(
            public_inputs,
            PrivateInputs::new_proof_of_core_quota_inputs(key_index, self.0.clone()),
        ))
    }
}
