use async_trait::async_trait;
use lb_cryptarchia_engine::Epoch;
use lb_log_targets::blend;

use crate::message_blend::{
    CoreProofOfQuotaGenerator,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        core_and_leader::{CoreAndLeaderProofsGenerator as _, RealCoreAndLeaderProofsGenerator},
        pow::{PowProofsGenerator as _, RealPowProofsGenerator},
    },
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::proofs::CORE_LEADER_AND_POW;

/// Proof generator for all three `PoQ` variants.
///
/// It covers the same ground as the core and leadership generator plus the
/// proof of work branch, which needs neither stake nor an SDP declaration and
/// is therefore available to a node whose core and leadership quotas are
/// exhausted or were never granted. The three branches are indistinguishable to
/// a verifier, so which one backs a given message is a local decision.
#[async_trait]
pub trait CoreLeaderAndPowProofsGenerator<CorePoQGenerator>: Sized {
    /// Instantiate a new generator for the duration of an epoch.
    fn new(
        settings: ProofsGeneratorSettings,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self;
    /// Notify the proof generator about the stream of winning `PoL` slots for
    /// an epoch (one item per winning slot). After this is provided for a
    /// new epoch, the generator can provide leadership `PoQ` variants,
    /// pulling a fresh slot for each data message so each gets a distinct
    /// key nullifier.
    fn set_epoch_private(
        &mut self,
        winning_pol_info_stream: WinningPolInfoStream,
        reference_epoch: Epoch,
    );
    /// Request a new core proof from the prover. It returns `None` if the
    /// maximum core quota has already been reached for this epoch.
    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof>;
    /// Request a new leadership proof from the prover. It returns `None` if no
    /// secret `PoL` info has been provided for the current epoch or if all the
    /// winning slots for the current epoch have been used up.
    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof>;
    /// Request a new proof of work backed proof from the prover. It returns
    /// `None` if the epoch's `PoW` public inputs admit no proof at all.
    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof>;
    /// Stop the background work this generator is performing for its epoch.
    ///
    /// Called on the outgoing generator at an epoch rotation: it stays alive
    /// through the transition period to verify messages still in flight, but
    /// must not go on mining for an epoch that has ended.
    fn stop_proof_generation(&mut self);
}

pub struct RealCoreLeaderAndPowProofsGenerator<CorePoQGenerator> {
    core_and_leader_proofs_generator: RealCoreAndLeaderProofsGenerator<CorePoQGenerator>,
    pow_proofs_generator: RealPowProofsGenerator,
}

#[async_trait]
impl<CorePoQGenerator> CoreLeaderAndPowProofsGenerator<CorePoQGenerator>
    for RealCoreLeaderAndPowProofsGenerator<CorePoQGenerator>
where
    CorePoQGenerator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    fn new(
        settings: ProofsGeneratorSettings,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self {
            core_and_leader_proofs_generator: RealCoreAndLeaderProofsGenerator::new(
                settings,
                core_proof_of_quota_generator,
            ),
            // The `PoW` branch depends only on public epoch information, so
            // unlike the leadership branch it is ready from the moment the
            // generator is created.
            pow_proofs_generator: RealPowProofsGenerator::new(settings),
        }
    }

    fn set_epoch_private(
        &mut self,
        winning_pol_info_stream: WinningPolInfoStream,
        reference_epoch: Epoch,
    ) {
        self.core_and_leader_proofs_generator
            .set_epoch_private(winning_pol_info_stream, reference_epoch);
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        self.core_and_leader_proofs_generator
            .get_next_core_proof()
            .await
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        self.core_and_leader_proofs_generator
            .get_next_leader_proof()
            .await
    }

    fn stop_proof_generation(&mut self) {
        // Only the `PoW` branch mines in the background; the other two produce
        // a proof when one is asked for and idle otherwise.
        self.pow_proofs_generator.stop();
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        let proof = self.pow_proofs_generator.get_next_proof().await?;
        tracing::trace!(
            target: LOG_TARGET,
            epoch = ?self.pow_proofs_generator.settings.epoch,
            quota = %self.pow_proofs_generator.settings.public_inputs.pow.pow_quota,
            membership_size = self.pow_proofs_generator.settings.membership_size,
            local_node_index = ?self.pow_proofs_generator.settings.local_node_index,
            key_nullifier = ?proof.proof_of_quota.key_nullifier(),
            signing_key = ?proof.ephemeral_signing_key.public_key(),
            "generated PoW PoQ"
        );
        Some(proof)
    }
}
