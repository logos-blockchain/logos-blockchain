use core::pin::Pin;

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt as _};
use key_management_system_keys::keys::UnsecuredEd25519Key;
use nomos_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use nomos_blend_proofs::{
    quota::inputs::prove::{PublicInputs, public::LeaderInputs},
    selection::VerifiedProofOfSelection,
};

use crate::message_blend::{
    CoreProofOfQuotaGenerator,
    provers::{BlendLayerProof, ProofsGeneratorSettings},
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = "blend::scheduling::proofs::core";
const PROOFS_GENERATOR_BUFFER_SIZE: usize = 10;

/// Proof generator for core `PoQ` variants.
#[async_trait]
pub trait CoreProofsGenerator<PoQGenerator>: Sized {
    /// Instantiate a new generator for the duration of a session.
    fn new(settings: ProofsGeneratorSettings, proof_of_quota_generator: PoQGenerator) -> Self;
    /// Notify the proof generator that a new epoch has started mid-session.
    /// This will trigger proof re-generation due to the change in the set of
    /// public inputs.
    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs);
    /// Request a new core proof from the prover. It returns `None` if the
    /// maximum core quota has already been reached for this session.
    async fn get_next_proof(&mut self) -> Option<BlendLayerProof>;
}

pub struct RealCoreProofsGenerator<PoQGenerator> {
    remaining_quota: u64,
    pub(super) settings: ProofsGeneratorSettings,
    pub(super) proof_of_quota_generator: PoQGenerator,
    proof_stream: Pin<Box<dyn Stream<Item = BlendLayerProof> + Send + Sync>>,
}

#[async_trait]
impl<PoQGenerator> CoreProofsGenerator<PoQGenerator> for RealCoreProofsGenerator<PoQGenerator>
where
    PoQGenerator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    fn new(settings: ProofsGeneratorSettings, proof_of_quota_generator: PoQGenerator) -> Self {
        Self {
            proof_stream: Box::pin(create_proof_stream(
                settings.public_inputs,
                proof_of_quota_generator.clone(),
                0,
            )),
            proof_of_quota_generator,
            remaining_quota: settings.public_inputs.core.quota,
            settings,
        }
    }

    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs) {
        tracing::info!(target: LOG_TARGET, "Rotating epoch...");

        // On epoch rotation, we maintain the remaining session quota for core proofs
        // and we only update the PoL part of the public inputs, before regenerating all
        // proofs.
        self.settings.public_inputs.leader = new_epoch_public;
        let next_key_index = self
            .settings
            .public_inputs
            .core
            .quota
            .checked_sub(self.remaining_quota)
            .expect("Remaining quota should never be larger than total quota.");

        // Compute new proofs with the updated settings.
        self.generate_new_proofs_stream(next_key_index);
    }

    async fn get_next_proof(&mut self) -> Option<BlendLayerProof> {
        self.remaining_quota = self.remaining_quota.checked_sub(1)?;
        let proof = self.proof_stream.next().await?;
        tracing::trace!(target: LOG_TARGET, "Generated core Blend layer proof with key nullifier {:?} addressed to node at index {:?}", proof.proof_of_quota.key_nullifier(), proof.proof_of_selection.expected_index(self.settings.membership_size));
        Some(proof)
    }
}

impl<PoQGenerator> RealCoreProofsGenerator<PoQGenerator>
where
    PoQGenerator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    // This will kill the previous running task, if any, since we swap the receiver
    // channel, hence the old task will fail to send new proofs and will abort on
    // its own.
    fn generate_new_proofs_stream(&mut self, starting_key_index: u64) {
        if self.remaining_quota == 0 {
            return;
        }

        self.proof_stream = Box::pin(create_proof_stream(
            self.settings.public_inputs,
            self.proof_of_quota_generator.clone(),
            starting_key_index,
        ));
    }
}

fn create_proof_stream<Generator>(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    proof_of_quota_generator: Generator,
    starting_key_index: u64,
) -> impl Stream<Item = BlendLayerProof>
where
    Generator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    let proofs_to_generate = public_inputs
        .core
        .quota
        .checked_sub(starting_key_index)
        .expect("Starting key index should never be larger than core quota.");
    tracing::trace!(target: LOG_TARGET, "Generating {proofs_to_generate} core quota proofs starting from index: {starting_key_index}.");

    let quota = public_inputs.core.quota;
    stream::iter(starting_key_index..quota)
        .map(move |key_index| {
            let ephemeral_signing_key = UnsecuredEd25519Key::generate_with_blake_rng();
            let proof_of_quota_generator = proof_of_quota_generator.clone();

            async move {
                let (proof_of_quota, secret_selection_randomness) = proof_of_quota_generator
                    .generate_poq(
                        &PublicInputs {
                            signing_key: ephemeral_signing_key.public_key().into_inner(),
                            core: public_inputs.core,
                            leader: public_inputs.leader,
                            session: public_inputs.session,
                        },
                        key_index,
                    )
                    .await
                    .ok()?;
                let proof_of_selection = VerifiedProofOfSelection::new(secret_selection_randomness);
                Some(BlendLayerProof {
                    proof_of_quota,
                    proof_of_selection,
                    ephemeral_signing_key,
                })
            }
        })
        .buffered(PROOFS_GENERATOR_BUFFER_SIZE)
        .filter_map(async |result| result)
}
