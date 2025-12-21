use core::mem::swap;

use key_management_system_keys::keys::Ed25519PublicKey;
use nomos_blend_proofs::{
    quota::{
        self, ProofOfQuota, VerifiedProofOfQuota,
        inputs::prove::{
            PublicInputs,
            public::{CoreInputs, LeaderInputs},
        },
    },
    selection::{self, ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::encap::ProofsVerifier;

/// The inputs required to verify a Proof of Quota, without the signing key,
/// which is retrieved from the public header of the message layer being
/// verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoQVerificationInputsMinusSigningKey {
    pub session: u64,
    pub core: CoreInputs,
    pub leader: LeaderInputs,
}

#[cfg(test)]
impl Default for PoQVerificationInputsMinusSigningKey {
    fn default() -> Self {
        use nomos_core::crypto::ZkHash;

        Self {
            session: 1,
            core: CoreInputs {
                zk_root: ZkHash::default(),
                quota: 1,
            },
            leader: LeaderInputs {
                pol_ledger_aged: ZkHash::default(),
                pol_epoch_nonce: ZkHash::default(),
                message_quota: 1,
                total_stake: 1,
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid Proof of Quota: {0}.")]
    ProofOfQuota(#[from] quota::Error),
    #[error("Invalid Proof of Selection: {0}.")]
    ProofOfSelection(selection::Error),
}

/// Verifier that actually verifies the validity of Blend-related proofs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealProofsVerifier {
    current_inputs: PoQVerificationInputsMinusSigningKey,
    previous_epoch_inputs: Option<LeaderInputs>,
}

impl ProofsVerifier for RealProofsVerifier {
    type Error = Error;

    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self {
            current_inputs: public_inputs,
            previous_epoch_inputs: None,
        }
    }

    fn start_epoch_transition(&mut self, new_pol_inputs: LeaderInputs) {
        let old_epoch_inputs = {
            let mut new_pol_inputs = new_pol_inputs;
            swap(&mut self.current_inputs.leader, &mut new_pol_inputs);
            new_pol_inputs
        };
        self.previous_epoch_inputs = Some(old_epoch_inputs);
    }

    fn complete_epoch_transition(&mut self) {
        self.previous_epoch_inputs = None;
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        let PoQVerificationInputsMinusSigningKey {
            core,
            leader,
            session,
        } = self.current_inputs;

        // Try with current input, and if it fails, try with the previous one, if any
        // (i.e., within the epoch transition period).
        proof
            .verify(&PublicInputs {
                core,
                leader,
                session,
                signing_key: *signing_key.as_inner(),
            })
            .or_else(|_| {
                let Some(previous_epoch_inputs) = self.previous_epoch_inputs else {
                    return Err(Error::ProofOfQuota(quota::Error::InvalidProof));
                };
                proof
                    .verify(&PublicInputs {
                        core,
                        leader: previous_epoch_inputs,
                        session,
                        signing_key: *signing_key.as_inner(),
                    })
                    .map_err(Error::ProofOfQuota)
            })
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        proof.verify(inputs).map_err(Error::ProofOfSelection)
    }
}
