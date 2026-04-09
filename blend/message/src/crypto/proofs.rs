use core::mem::swap;
use std::time::Instant;

use lb_blend_proofs::{
    quota::{
        self, ProofOfQuota, VerifiedProofOfQuota,
        inputs::prove::{
            PublicInputs,
            public::{CoreInputs, LeaderInputs},
        },
    },
    selection::{self, ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
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
        use lb_core::crypto::ZkHash;
        use lb_groth16::{Field as _, Fr};

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
                lottery_0: Fr::ZERO,
                lottery_1: Fr::ZERO,
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
        tracing::trace!("Generating new proof verifier with public inputs: {public_inputs:?}");
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
        tracing::trace!(
            "Transitioning epochs for proof verifier from: {old_epoch_inputs:?} to: {new_pol_inputs:?}"
        );
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
        tracing::trace!(
            "Verifying proof of quota with key nullifier {:?}, signing key: {signing_key:?}, session {session:?}, public core inputs: {core:?} and leader inputs: {leader:?}.",
            hex::encode(fr_to_bytes(&proof.key_nullifier()))
        );
        let start = Instant::now();
        let proof_verification_result = proof
            .verify(&PublicInputs {
                core,
                leader,
                session,
                signing_key: *signing_key.as_inner(),
            })
            .or_else(|_| {
                let Some(previous_epoch_inputs) = self.previous_epoch_inputs else {
                    tracing::debug!("Input proof invalid and no previous epoch to try with");
                    return Err(Error::ProofOfQuota(quota::Error::InvalidProof));
                };
                tracing::trace!(
                    "Verifying same proof of quota with previous epoch leader inputs: {previous_epoch_inputs:?}."
                );
                proof
                    .verify(&PublicInputs {
                        core,
                        leader: previous_epoch_inputs,
                        session,
                        signing_key: *signing_key.as_inner(),
                    })
                    .map_err(Error::ProofOfQuota)
                    .inspect_err(|_| {
                        tracing::debug!(
                            "Input proof invalid with both current and previous epoch public inputs"
                        );
                    })
            });

        tracing::trace!(
            "Proof verification time: {} ms.",
            start.elapsed().as_millis()
        );

        proof_verification_result
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        proof.verify(inputs).map_err(Error::ProofOfSelection)
    }
}

#[cfg(test)]
mod tests {
    use lb_blend_proofs::quota::inputs::prove::public::LeaderInputs;
    use lb_core::crypto::ZkHash;
    use lb_groth16::{Field as _, Fr};

    use crate::{
        crypto::proofs::{PoQVerificationInputsMinusSigningKey, RealProofsVerifier},
        encap::ProofsVerifier as _,
    };

    fn epoch_1_leader() -> LeaderInputs {
        LeaderInputs {
            pol_ledger_aged: ZkHash::ONE,
            pol_epoch_nonce: ZkHash::ONE,
            message_quota: 2,
            lottery_0: Fr::ONE,
            lottery_1: Fr::ONE,
        }
    }

    #[test]
    fn new_verifier_has_no_previous_epoch() {
        let verifier = RealProofsVerifier::new(PoQVerificationInputsMinusSigningKey::default());
        assert!(verifier.previous_epoch_inputs.is_none());
        assert_eq!(
            verifier.current_inputs.leader,
            PoQVerificationInputsMinusSigningKey::default().leader
        );
    }

    #[test]
    fn start_epoch_transition_stores_previous_epoch() {
        let initial = PoQVerificationInputsMinusSigningKey::default();
        let mut verifier = RealProofsVerifier::new(initial);
        let new_leader = epoch_1_leader();

        verifier.start_epoch_transition(new_leader);

        // Current should be updated to new epoch.
        assert_eq!(verifier.current_inputs.leader, new_leader);
        // Previous should hold the old epoch's leader inputs.
        assert_eq!(verifier.previous_epoch_inputs, Some(initial.leader));
    }

    #[test]
    fn complete_epoch_transition_clears_previous_epoch() {
        let mut verifier = RealProofsVerifier::new(PoQVerificationInputsMinusSigningKey::default());
        verifier.start_epoch_transition(epoch_1_leader());

        assert!(verifier.previous_epoch_inputs.is_some());

        verifier.complete_epoch_transition();

        assert!(
            verifier.previous_epoch_inputs.is_none(),
            "Previous epoch inputs must be cleared after completing transition"
        );
        assert_eq!(verifier.current_inputs.leader, epoch_1_leader());
    }

    #[test]
    fn consecutive_epoch_transitions_replace_previous() {
        let initial = PoQVerificationInputsMinusSigningKey::default();
        let mut verifier = RealProofsVerifier::new(initial);

        let leader_1 = epoch_1_leader();
        verifier.start_epoch_transition(leader_1);
        assert_eq!(verifier.previous_epoch_inputs, Some(initial.leader));

        // Start another transition without completing the first.
        let leader_2 = LeaderInputs {
            pol_ledger_aged: ZkHash::ZERO,
            pol_epoch_nonce: ZkHash::ONE,
            message_quota: 3,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ONE,
        };
        verifier.start_epoch_transition(leader_2);

        // Previous should now be epoch 1 (not initial epoch 0).
        assert_eq!(verifier.current_inputs.leader, leader_2);
        assert_eq!(verifier.previous_epoch_inputs, Some(leader_1));
    }

    #[test]
    fn complete_then_new_epoch_transition() {
        let initial = PoQVerificationInputsMinusSigningKey::default();
        let mut verifier = RealProofsVerifier::new(initial);

        // Epoch 0 → 1
        let leader_1 = epoch_1_leader();
        verifier.start_epoch_transition(leader_1);
        verifier.complete_epoch_transition();
        assert!(verifier.previous_epoch_inputs.is_none());
        assert_eq!(verifier.current_inputs.leader, leader_1);

        // Epoch 1 → 2
        let leader_2 = LeaderInputs {
            pol_ledger_aged: ZkHash::ZERO,
            pol_epoch_nonce: ZkHash::ONE,
            message_quota: 3,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ONE,
        };
        verifier.start_epoch_transition(leader_2);
        assert_eq!(verifier.current_inputs.leader, leader_2);
        assert_eq!(
            verifier.previous_epoch_inputs,
            Some(leader_1),
            "After new transition, previous must be the completed epoch 1"
        );
    }

    #[test]
    fn session_and_core_inputs_preserved_across_epoch_transitions() {
        let initial = PoQVerificationInputsMinusSigningKey::default();
        let mut verifier = RealProofsVerifier::new(initial);

        verifier.start_epoch_transition(epoch_1_leader());

        // Session and core inputs should not change during epoch transitions.
        assert_eq!(verifier.current_inputs.session, initial.session);
        assert_eq!(verifier.current_inputs.core, initial.core);
    }
}
