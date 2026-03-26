use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt as _};
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
use tokio::{
    sync::mpsc,
    task::{JoinHandle, spawn_blocking},
    time::Instant,
};

use crate::message_blend::provers::{BlendLayerProof, ProofsGeneratorSettings};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = "blend::scheduling::proofs::leader";

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
    private_inputs: ProofOfLeadershipQuotaInputs,
    proof_receiver: mpsc::Receiver<BlendLayerProof>,
    proof_generation_task_handle: JoinHandle<()>,
}

impl Drop for RealLeaderProofsGenerator {
    fn drop(&mut self) {
        self.proof_generation_task_handle.abort();
    }
}

#[async_trait]
impl LeaderProofsGenerator for RealLeaderProofsGenerator {
    fn new(
        settings: ProofsGeneratorSettings,
        private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        let (proof_receiver, proof_generation_task_handle) = spawn_proof_generation(
            create_proof_stream(settings.public_inputs, private_inputs),
            settings.encapsulation_layers.get() as usize,
        );

        Self {
            settings,
            private_inputs,
            proof_receiver,
            proof_generation_task_handle,
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
        self.private_inputs = new_private;

        // Compute new proofs with the updated settings.
        self.generate_new_proofs_stream();
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        let start = Instant::now();
        let proof = self
            .proof_receiver
            .recv()
            .await
            .expect("Underlying proof generation task should always yield items.");
        tracing::trace!(target: LOG_TARGET, "Generated leadership Blend layer proof with key nullifier {:?} addressed to node at index {:?} in {:?} ms.", hex::encode(fr_to_bytes(&proof.proof_of_quota.key_nullifier())), proof.proof_of_selection.expected_index(self.settings.membership_size), start.elapsed().as_millis());
        proof
    }
}

impl RealLeaderProofsGenerator {
    fn generate_new_proofs_stream(&mut self) {
        self.proof_generation_task_handle.abort();

        let (proof_receiver, generation_task) = spawn_proof_generation(
            create_proof_stream(self.settings.public_inputs, self.private_inputs),
            self.settings.encapsulation_layers.get() as usize,
        );
        self.proof_receiver = proof_receiver;
        self.proof_generation_task_handle = generation_task;
    }

    pub(super) const fn current_epoch(&self) -> Epoch {
        self.settings.epoch
    }
}

// Spawns a background task that eagerly drives the proof stream, sending
// generated proofs into a bounded channel. This ensures proofs are
// pre-generated and ready for immediate consumption, rather than being lazily
// produced only when polled as is the case with a buffered stream.
fn spawn_proof_generation(
    stream: impl Stream<Item = BlendLayerProof> + Send + 'static,
    buffer_size: usize,
) -> (mpsc::Receiver<BlendLayerProof>, JoinHandle<()>) {
    let (proof_sender, proof_receiver) = mpsc::channel(buffer_size);
    let handle = tokio::spawn(async move {
        tokio::pin!(stream);
        while let Some(proof) = stream.next().await {
            if proof_sender.send(proof).await.is_err() {
                break;
            }
        }
    });
    (proof_receiver, handle)
}

#[expect(
    clippy::large_types_passed_by_value,
    reason = "Spawning an async task. Issues with lifetimes."
)]
fn create_proof_stream(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    private_inputs: ProofOfLeadershipQuotaInputs,
) -> impl Stream<Item = BlendLayerProof> {
    let message_quota = public_inputs.leader.message_quota;
    tracing::debug!(target: LOG_TARGET, "Generating leadership quota proofs starting with public inputs: {public_inputs:?}.");

    stream::iter(0u64..)
        .then(move |current_index| {
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

            async move {
                let leadership_proof = spawn_blocking(move || {
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
                    BlendLayerProof {
                        proof_of_quota,
                        proof_of_selection,
                        ephemeral_signing_key,
                    }
                }).await.expect("Spawning task for leadership proof generation should not fail.");

                tracing::trace!(target: LOG_TARGET, "Generated leadership PoQ within the stream for message release index {message_release_index:?} with key nullifier {:?}  and public key {:?}.", hex::encode(fr_to_bytes(&leadership_proof.proof_of_quota.key_nullifier())), leadership_proof.ephemeral_signing_key.public_key());
                leadership_proof
            }
        })
}
