use core::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt as _, stream};
use key_management_system_keys::keys::UnsecuredEd25519Key;
use nomos_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use nomos_blend_proofs::{
    quota::{
        VerifiedProofOfQuota,
        inputs::prove::{
            PrivateInputs, PublicInputs, private::ProofOfLeadershipQuotaInputs,
            public::LeaderInputs,
        },
    },
    selection::VerifiedProofOfSelection,
};
use tokio::task::spawn_blocking;

use crate::message_blend::provers::{BlendLayerProof, ProofsGeneratorSettings};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = "blend::scheduling::proofs::leader";
const PROOFS_GENERATOR_BUFFER_SIZE: usize = 10;

/// A `PoQ` generator that deals only with leadership proofs, suitable for edge
/// nodes.
#[async_trait]
pub trait LeaderProofsGenerator: Sized {
    /// Instantiate a new generator with the provided public inputs and secret
    /// `PoL` values.
    fn new(settings: ProofsGeneratorSettings, private_inputs: ProofOfLeadershipQuotaInputs)
    -> Self;
    /// Signal an epoch transition in the middle of the current session, with
    /// new public and secret inputs.
    fn rotate_epoch(
        &mut self,
        new_epoch_public: LeaderInputs,
        new_private_inputs: ProofOfLeadershipQuotaInputs,
    );
    /// Get the next leadership proof.
    async fn get_next_proof(&mut self) -> BlendLayerProof;
}

pub struct RealLeaderProofsGenerator {
    pub(super) settings: ProofsGeneratorSettings,
    proof_stream: Pin<Box<dyn Stream<Item = BlendLayerProof> + Send + Sync>>,
}

#[async_trait]
impl LeaderProofsGenerator for RealLeaderProofsGenerator {
    fn new(
        settings: ProofsGeneratorSettings,
        private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        Self {
            proof_stream: Box::pin(create_leadership_proof_stream(
                settings.public_inputs,
                private_inputs,
            )),
            settings,
        }
    }

    fn rotate_epoch(
        &mut self,
        new_epoch_public: LeaderInputs,
        new_private: ProofOfLeadershipQuotaInputs,
    ) {
        tracing::info!(target: LOG_TARGET, "Rotating epoch...");

        // On epoch rotation, we maintain the current session info and only change the
        // PoL relevant parts.
        self.settings.public_inputs.leader = new_epoch_public;

        // Compute new proofs with the updated settings.
        self.generate_new_proofs_stream(new_private);
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        let proof = self
            .proof_stream
            .next()
            .await
            .expect("Underlying proof generation stream should always yield items.");
        tracing::trace!(target: LOG_TARGET, "Generated leadership Blend layer proof with key nullifier {:?} addressed to node at index {:?}", proof.proof_of_quota.key_nullifier(), proof.proof_of_selection.expected_index(self.settings.membership_size));
        proof
    }
}

impl RealLeaderProofsGenerator {
    fn generate_new_proofs_stream(&mut self, private_inputs: ProofOfLeadershipQuotaInputs) {
        self.proof_stream = Box::pin(create_leadership_proof_stream(
            self.settings.public_inputs,
            private_inputs,
        ));
    }
}

fn create_leadership_proof_stream(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    private_inputs: ProofOfLeadershipQuotaInputs,
) -> impl Stream<Item = BlendLayerProof> {
    let message_quota = public_inputs.leader.message_quota;

    stream::iter(0u64..)
        .map(move |current_index| {
            let encapsulation_layer = current_index % message_quota;
            let private_inputs = private_inputs.clone();

            spawn_blocking(move || {
                let ephemeral_signing_key = UnsecuredEd25519Key::generate_with_blake_rng();
                let (proof_of_quota, secret_selection_randomness) = VerifiedProofOfQuota::new(
                    &PublicInputs {
                        signing_key: ephemeral_signing_key.public_key().into_inner(),
                        core: public_inputs.core,
                        leader: public_inputs.leader,
                        session: public_inputs.session,
                    },
                    PrivateInputs::new_proof_of_leadership_quota_inputs(
                        encapsulation_layer,
                        private_inputs,
                    ),
                )
                .ok()?;
                let proof_of_selection = VerifiedProofOfSelection::new(secret_selection_randomness);
                Some(BlendLayerProof {
                    proof_of_quota,
                    proof_of_selection,
                    ephemeral_signing_key,
                })
            })
        })
        .buffered(PROOFS_GENERATOR_BUFFER_SIZE)
        .filter_map(async |result| result.ok().flatten())
}
