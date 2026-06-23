use async_trait::async_trait;
use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
use lb_cryptarchia_engine::Epoch;
use lb_log_targets::blend;

use crate::message_blend::{
    CoreProofOfQuotaGenerator,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        core::{CoreProofsGenerator as _, RealCoreProofsGenerator},
        leader::{LeaderProofsGenerator as _, RealLeaderProofsGenerator},
    },
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::proofs::CORE_AND_LEADER;

/// Proof generator for core and leader `PoQ` variants.
///
/// Because leader `PoQ` variants require secret `PoL` info, and because a core
/// node with very little stake might not even have a winning slot for a given
/// epoch, the process of providing secret `PoL` info is different from that of
/// providing new (public) epoch information, so as not to block cover message
/// generation for those nodes with low stake.
#[async_trait]
pub trait CoreAndLeaderProofsGenerator<CorePoQGenerator>: Sized {
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
}

pub struct RealCoreAndLeaderProofsGenerator<CorePoQGenerator> {
    core_proofs_generator: RealCoreProofsGenerator<CorePoQGenerator>,
    leader_proofs_generator: Option<RealLeaderProofsGenerator>,
}

impl<CorePoQGenerator> RealCoreAndLeaderProofsGenerator<CorePoQGenerator> {
    #[cfg(test)]
    pub const fn override_settings(&mut self, new_settings: ProofsGeneratorSettings) {
        self.core_proofs_generator.settings = new_settings;
        if let Some(leader_proofs_generator) = &mut self.leader_proofs_generator {
            leader_proofs_generator.settings = new_settings;
        }
    }
}

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for RealCoreAndLeaderProofsGenerator<CorePoQGenerator>
where
    CorePoQGenerator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    fn new(
        settings: ProofsGeneratorSettings,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self {
            core_proofs_generator: RealCoreProofsGenerator::new(
                settings,
                core_proof_of_quota_generator,
            ),
            leader_proofs_generator: None,
        }
    }

    // Creates a new leader proofs generator with the provided public inputs and
    // winning-slot stream.
    fn set_epoch_private(
        &mut self,
        winning_pol_info_stream: WinningPolInfoStream,
        reference_epoch: Epoch,
    ) {
        // TODO: Change trait API to avoid runtime panics.
        let (current_generator_epoch, current_leader_inputs) = (
            self.core_proofs_generator.settings.epoch,
            self.core_proofs_generator.settings.public_inputs.leader,
        );
        assert!(
            current_generator_epoch == reference_epoch,
            "set_epoch_private should be called with a reference epoch matching the current core proofs generator's epoch."
        );
        let current_epoch_local_node_index = self.core_proofs_generator.settings.local_node_index;
        let current_epoch_membership_size = self.core_proofs_generator.settings.membership_size;
        let current_epoch_core_public_inputs =
            self.core_proofs_generator.settings.public_inputs.core;

        self.leader_proofs_generator = Some(RealLeaderProofsGenerator::new(
            ProofsGeneratorSettings {
                epoch: reference_epoch,
                local_node_index: current_epoch_local_node_index,
                membership_size: current_epoch_membership_size,
                public_inputs: PoQVerificationInputsMinusSigningKey {
                    core: current_epoch_core_public_inputs,
                    leader: current_leader_inputs,
                },
                encapsulation_layers: self.core_proofs_generator.settings.encapsulation_layers,
            },
            winning_pol_info_stream,
        ));
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        let proof = self.core_proofs_generator.get_next_proof().await?;
        tracing::trace!(
            target: LOG_TARGET,
            epoch = ?self.core_proofs_generator.settings.epoch,
            quota = self.core_proofs_generator.settings.public_inputs.core.quota,
            membership_size = self.core_proofs_generator.settings.membership_size,
            local_node_index = ?self.core_proofs_generator.settings.local_node_index,
            key_nullifier = ?proof.proof_of_quota.key_nullifier(),
            signing_key = ?proof.ephemeral_signing_key.public_key(),
            "generated core PoQ"
        );
        Some(proof)
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        let Some(leader_proofs_generator) = &mut self.leader_proofs_generator else {
            return None;
        };
        let proof = leader_proofs_generator.get_next_proof().await?;
        tracing::trace!(
            target: LOG_TARGET,
            epoch = ?leader_proofs_generator.settings.epoch,
            quota = leader_proofs_generator.settings.public_inputs.core.quota,
            membership_size = leader_proofs_generator.settings.membership_size,
            local_node_index = ?leader_proofs_generator.settings.local_node_index,
            key_nullifier = ?proof.proof_of_quota.key_nullifier(),
            signing_key = ?proof.ephemeral_signing_key.public_key(),
            "generated leadership PoQ"
        );
        Some(proof)
    }
}
