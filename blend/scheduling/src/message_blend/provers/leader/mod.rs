use core::pin::Pin;

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt as _};
use lb_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use lb_blend_proofs::{
    quota::{
        VerifiedProofOfQuota,
        inputs::prove::{PrivateInputs, PublicInputs},
    },
    selection::VerifiedProofOfSelection,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;
use lb_log_targets::blend;
use lb_utils::tokio::stream::Buffered;
use tokio::{task::spawn_blocking, time::Instant};

use crate::message_blend::{
    buffer_size,
    provers::{BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream},
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::proofs::LEADER;

/// A `PoQ` generator that deals only with leadership proofs, suitable for edge
/// nodes.
#[async_trait]
pub trait LeaderProofsGenerator: Sized {
    /// Instantiate a new generator with the provided public inputs and a stream
    /// of winning-slot secret `PoL` values (one item per winning slot).
    fn new(
        settings: ProofsGeneratorSettings,
        winning_pol_info_stream: WinningPolInfoStream,
    ) -> Self;
    /// Get the next leadership proof.
    async fn get_next_proof(&mut self) -> Option<BlendLayerProof>;
}

pub struct RealLeaderProofsGenerator {
    pub(super) settings: ProofsGeneratorSettings,
    proofs_stream: Pin<Box<dyn Stream<Item = BlendLayerProof> + Send + Sync>>,
}

impl RealLeaderProofsGenerator {
    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.settings.epoch
    }
}

#[async_trait]
impl LeaderProofsGenerator for RealLeaderProofsGenerator {
    fn new(
        settings: ProofsGeneratorSettings,
        winning_pol_info_stream: WinningPolInfoStream,
    ) -> Self {
        Self {
            settings,
            proofs_stream: Box::pin(create_proof_stream(
                settings.public_inputs,
                winning_pol_info_stream,
                buffer_size(settings.public_inputs.leader.message_quota as usize),
            )),
        }
    }

    async fn get_next_proof(&mut self) -> Option<BlendLayerProof> {
        let start = Instant::now();
        let Some(proof) = self.proofs_stream.next().await else {
            tracing::warn!(target: LOG_TARGET, "Leadership proof stream ended. No proof is generated.");
            return None;
        };
        tracing::trace!(target: LOG_TARGET, "Generated leadership Blend layer proof with key nullifier {:?} addressed to node at index {:?} in {:?} ms.", hex::encode(fr_to_bytes(&proof.proof_of_quota.key_nullifier())), proof.proof_of_selection.expected_index(self.settings.membership_size), start.elapsed().as_millis());
        Some(proof)
    }
}

fn create_proof_stream(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    winning_pol_info_stream: WinningPolInfoStream,
    buffer_size: usize,
) -> impl Stream<Item = BlendLayerProof> + Send {
    let message_quota = public_inputs.leader.message_quota;
    tracing::debug!(target: LOG_TARGET, "Generating leadership quota proofs starting with public inputs: {public_inputs:?}.");

    // Each winning slot from `winning_pol_info_stream` yields exactly
    // `message_quota` proofs (one original data message's worth:
    // `num_blend_layers` layers times the message copies given by the data
    // replication factor), indexed `0..message_quota`. The key nullifier is a
    // function of the (slot, message index) pair, so the `message_quota` proofs
    // of one slot get distinct nullifiers, and consecutive messages use
    // distinct slots and therefore distinct nullifiers. The mapping of indices
    // to each message + encapsulation layer is up to the scheduler.
    Buffered::new(
        winning_pol_info_stream.flat_map(move |slot_inputs| {
            stream::iter(0..message_quota).map(move |message_release_index| {
                let slot_inputs = slot_inputs.clone();

                // Spawn eagerly here (outside `async move`) so the blocking task starts as
                // soon as the stream buffer slot is filled, not when the future is first polled.
                // Without this, `spawn_blocking` would only be called when `FuturesOrdered`
                // first polls the future — which only happens when the consumer polls the
                // stream — causing avoidable latency when the consumer is idle.
                let task = spawn_blocking(move || {
                    let ephemeral_signing_key = UnsecuredEd25519Key::generate_with_blake_rng();
                    let (proof_of_quota, secret_selection_randomness) = VerifiedProofOfQuota::new(
                        &PublicInputs {
                            signing_key: ephemeral_signing_key.public_key().into_inner(),
                            core: public_inputs.core,
                            leader: public_inputs.leader,
                        },
                        PrivateInputs::new_proof_of_leadership_quota_inputs(
                            message_release_index,
                            slot_inputs,
                        ),
                    )
                    .expect("Leadership PoQ proof creation should not fail.");
                    let proof_of_selection = VerifiedProofOfSelection::new(secret_selection_randomness);
                    BlendLayerProof {
                        proof_of_quota,
                        proof_of_selection,
                        ephemeral_signing_key,
                    }
                });

                async move {
                    let leadership_proof = task.await.expect("Spawning task for leadership proof generation should not fail.");

                    tracing::trace!(target: LOG_TARGET, "Generated leadership PoQ within the stream for message release index {message_release_index:?} with key nullifier {:?}  and public key {:?}.", hex::encode(fr_to_bytes(&leadership_proof.proof_of_quota.key_nullifier())), leadership_proof.ephemeral_signing_key.public_key());
                    leadership_proof
                }
            })
        }),
        buffer_size,
    )
}
