use core::{marker::PhantomData, pin::Pin};

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt as _};
use lb_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use lb_blend_proofs::{quota::inputs::prove::PublicInputs, selection::VerifiedProofOfSelection};
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;
use lb_log_targets::blend;
use lb_utils::tokio::stream::Buffered;
use tokio::{task::spawn, time::Instant};

use crate::message_blend::{
    CoreProofOfQuotaGenerator, buffer_size,
    provers::{BlendLayerProof, ProofsGeneratorSettings},
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::proofs::CORE;

/// Proof generator for core `PoQ` variants.
#[async_trait]
pub trait CoreProofsGenerator<PoQGenerator>: Sized {
    /// Instantiate a new generator for the duration of an epoch.
    fn new(settings: ProofsGeneratorSettings, proof_of_quota_generator: PoQGenerator) -> Self;
    /// Request a new core proof from the prover. It returns `None` if the
    /// maximum core quota has already been reached for this epoch.
    async fn get_next_proof(&mut self) -> Option<BlendLayerProof>;
}

pub struct RealCoreProofsGenerator<PoQGenerator> {
    remaining_quota: u64,
    pub(super) settings: ProofsGeneratorSettings,
    proofs_stream: Pin<Box<dyn Stream<Item = BlendLayerProof> + Send + Sync>>,
    _phantom: PhantomData<PoQGenerator>,
}

#[async_trait]
impl<PoQGenerator> CoreProofsGenerator<PoQGenerator> for RealCoreProofsGenerator<PoQGenerator>
where
    PoQGenerator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    fn new(settings: ProofsGeneratorSettings, proof_of_quota_generator: PoQGenerator) -> Self {
        Self {
            proofs_stream: Box::pin(create_proof_stream(
                settings.public_inputs,
                proof_of_quota_generator,
                0,
                buffer_size(settings.encapsulation_layers.get() as usize),
            )),
            remaining_quota: settings.public_inputs.core.quota,
            settings,
            _phantom: PhantomData,
        }
    }

    async fn get_next_proof(&mut self) -> Option<BlendLayerProof> {
        let start = Instant::now();
        let Some(remaining_quota) = self.remaining_quota.checked_sub(1) else {
            tracing::warn!(target: LOG_TARGET, "Core quota exhausted. No proof is generated.");
            return None;
        };
        self.remaining_quota = remaining_quota;
        let Some(proof) = self.proofs_stream.next().await else {
            tracing::warn!(target: LOG_TARGET, "No proof available from the stream.");
            return None;
        };
        tracing::trace!(target: LOG_TARGET, "Generated core Blend layer proof with key nullifier {:?} addressed to node at index {:?} in {:?} ms.", hex::encode(fr_to_bytes(&proof.proof_of_quota.key_nullifier())), proof.proof_of_selection.expected_index(self.settings.membership_size), start.elapsed().as_millis());
        Some(proof)
    }
}

fn create_proof_stream<Generator>(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    proof_of_quota_generator: Generator,
    starting_key_index: u64,
    buffer_size: usize,
) -> impl Stream<Item = BlendLayerProof> + Send
where
    Generator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    let proofs_to_generate = public_inputs
        .core
        .quota
        .checked_sub(starting_key_index)
        .expect("Starting key index should never be larger than core quota.");
    tracing::trace!(target: LOG_TARGET, "Generating {proofs_to_generate} core quota proofs starting from index: {starting_key_index} with public inputs: {public_inputs:?}.");

    let quota = public_inputs.core.quota;
    Buffered::new(
        stream::iter(starting_key_index..quota)
        .map(move |key_index| {
            let ephemeral_signing_key = UnsecuredEd25519Key::generate_with_blake_rng();
            let proof_of_quota_generator = proof_of_quota_generator.clone();

            // Spawn eagerly here (outside `async move`) so the task starts as soon as the
            // stream buffer slot is filled, not when the future is first polled.
            // Without this, `generate_poq` would only begin when `FuturesOrdered` first
            // polls the future — which only happens when the consumer polls the stream —
            // causing avoidable latency when the consumer is idle.
            let task = spawn(async move {
                let (proof_of_quota, secret_selection_randomness) = proof_of_quota_generator
                    .generate_poq(
                        &PublicInputs {
                            signing_key: ephemeral_signing_key.public_key().into_inner(),
                            core: public_inputs.core,
                            leader: public_inputs.leader,
                        },
                        key_index,
                    ).await.expect("Core PoQ generation should not fail.");

                let proof_of_selection = VerifiedProofOfSelection::new(secret_selection_randomness);
                tracing::trace!(target: LOG_TARGET, "Generated core PoQ within the stream for message release index {key_index:?} with key nullifier {:?} and public key {:?}.", hex::encode(fr_to_bytes(&proof_of_quota.key_nullifier())), ephemeral_signing_key.public_key());
                BlendLayerProof {
                    proof_of_quota,
                    proof_of_selection,
                    ephemeral_signing_key,
                }
            });

            async move { task.await.expect("Core PoQ generation task should not fail.") }
        }),
        buffer_size,
    )
}
