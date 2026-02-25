use core::{future::ready, pin::Pin};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _, stream};
use lb_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use lb_blend_proofs::{
    quota::{
        VerifiedProofOfQuota,
        inputs::prove::{
            PrivateInputs, PublicInputs, private::ProofOfLeadershipQuotaInputs,
            public::LeaderInputs,
        },
    },
    selection::VerifiedProofOfSelection,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;

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
        new_epoch: Epoch,
    );
    /// Get the next leadership proof.
    async fn get_next_proof(&mut self) -> BlendLayerProof;
}

pub struct RealLeaderProofsGenerator {
    pub(super) settings: ProofsGeneratorSettings,
    proof_stream: Pin<Box<dyn Stream<Item = BlendLayerProof> + Send + Sync>>,
    cancellation_token: CancellationToken,
}

#[async_trait]
impl LeaderProofsGenerator for RealLeaderProofsGenerator {
    fn new(
        settings: ProofsGeneratorSettings,
        private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        let cancellation_token = CancellationToken::new();

        Self {
            proof_stream: Box::pin(create_leadership_proof_stream(
                settings.public_inputs,
                private_inputs,
                cancellation_token.clone(),
            )),
            settings,
            cancellation_token,
        }
    }

    fn rotate_epoch(
        &mut self,
        new_epoch_public: LeaderInputs,
        new_private: ProofOfLeadershipQuotaInputs,
        new_epoch: Epoch,
    ) {
        tracing::info!(target: LOG_TARGET, "Rotating epoch...");

        // On epoch rotation, we maintain the current session info and only change the
        // PoL relevant parts.
        self.settings.public_inputs.leader = new_epoch_public;
        self.settings.epoch = new_epoch;

        // Compute new proofs with the updated settings.
        self.generate_new_proofs_stream(&new_private);
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        let proof = self
            .proof_stream
            .next()
            .await
            .expect("Underlying proof generation stream should always yield items.");
        tracing::trace!(target: LOG_TARGET, "Generated leadership Blend layer proof with key nullifier {:?} addressed to node at index {:?}", hex::encode(fr_to_bytes(&proof.proof_of_quota.key_nullifier())), proof.proof_of_selection.expected_index(self.settings.membership_size));
        proof
    }
}

impl RealLeaderProofsGenerator {
    fn generate_new_proofs_stream(&mut self, private_inputs: &ProofOfLeadershipQuotaInputs) {
        self.cancellation_token.cancel();

        let new_cancellation_token = CancellationToken::new();
        self.cancellation_token = new_cancellation_token.clone();

        self.proof_stream = Box::pin(create_leadership_proof_stream(
            self.settings.public_inputs,
            *private_inputs,
            new_cancellation_token,
        ));
    }

    pub(super) const fn current_epoch(&self) -> Epoch {
        self.settings.epoch
    }
}

#[expect(
    clippy::large_types_passed_by_value,
    reason = "Spawning an async task. Issues with lifetimes."
)]
fn create_leadership_proof_stream(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    private_inputs: ProofOfLeadershipQuotaInputs,
    cancellation_token: CancellationToken,
) -> impl Stream<Item = BlendLayerProof> {
    let message_quota = public_inputs.leader.message_quota;
    tracing::debug!(target: LOG_TARGET, "Generating leadership quota proofs starting with public inputs: {public_inputs:?}.");

    stream::iter(0u64..)
        // Stop producing new items once cancelled
        .take_while({
            let token = cancellation_token.clone();
            move |_| {
                let is_active = !token.is_cancelled();
                async move { is_active }
            }
        })
        .map(move |current_index| {
            // This represents the total number of encapsulations sent out for each message.
            // E.g., for a session with data message replication factor of `1`, we get
            // indices `0` to `2` that belong to the first copy encapsulation, and indices
            // `3` to `5` that belong to the second copy encapsulation.
            // In the end, because the expected maximum message quota is `6` (if we take `3`
            // as the blending operations per message), we end up with two,
            // fully-encapsulated copies of the same original message, with valid proofs
            // because within the expected index value.
            // The logic on how these indices are mapped to each message + encapsulation
            // layer is out of scope for this component, and will be up to the
            // message scheduler.
            let message_release_index = current_index % message_quota;
            let token = cancellation_token.clone();

            async move {
                let token_clone = token.clone();
                let leadership_proof = spawn_blocking(move || {
                    if token_clone.is_cancelled() {
                        tracing::debug!(target: LOG_TARGET, "Leadership proof generation cancelled before starting.");
                        return None;
                    }

                    let ephemeral_signing_key = UnsecuredEd25519Key::generate_with_blake_rng();
                    let (proof_of_quota, secret_selection_randomness) = VerifiedProofOfQuota::new(
                        &PublicInputs {
                            signing_key: ephemeral_signing_key.public_key().into_inner(),
                            core: public_inputs.core,
                            leader: public_inputs.leader,
                            session: public_inputs.session,
                        },
                        PrivateInputs::new_proof_of_leadership_quota_inputs(
                            message_release_index,
                            private_inputs,
                        ),
                    )
                    .expect("Leadership PoQ proof creation should not fail.");
                    let proof_of_selection = VerifiedProofOfSelection::new(secret_selection_randomness);
                    Some(BlendLayerProof {
                        proof_of_quota,
                        proof_of_selection,
                        ephemeral_signing_key,
                    })
                }).await.expect("Spawning task for leadership proof generation should not fail.");

                if token.is_cancelled() {
                    tracing::debug!(target: LOG_TARGET, "Leadership proof generation cancelled after completion.");
                    return None;
                }
                leadership_proof
            }
        })
        .buffered(PROOFS_GENERATOR_BUFFER_SIZE)
        .filter_map(ready)
}
