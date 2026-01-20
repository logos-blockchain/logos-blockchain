use async_trait::async_trait;
use lb_blend_proofs::quota::inputs::prove::{
    private::ProofOfLeadershipQuotaInputs, public::LeaderInputs,
};

use crate::message_blend::{
    CoreProofOfQuotaGenerator,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings,
        core::{CoreProofsGenerator as _, RealCoreProofsGenerator},
        leader::{LeaderProofsGenerator as _, RealLeaderProofsGenerator},
    },
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = "blend::scheduling::proofs::core-and-leader";

/// Proof generator for core and leader `PoQ` variants.
///
/// Because leader `PoQ` variants require secret `PoL` info, and because a core
/// node with very little stake might not even have a winning slot for a given
/// epoch, the process of providing secret `PoL` info is different from that of
/// providing new (public) epoch information, so as not to block cover message
/// generation for those nodes with low stake.
#[async_trait]
pub trait CoreAndLeaderProofsGenerator<CorePoQGenerator>: Sized {
    /// Instantiate a new generator for the duration of a session.
    fn new(
        settings: ProofsGeneratorSettings,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self;
    /// Notify the proof generator that a new epoch has started mid-session.
    /// This will trigger core proof re-generation due to the change in the set
    /// of public inputs. Previously computed leader proofs are discarded and
    /// re-computation is halted until the new epoch private info are provided.
    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs);
    /// Notify the proof generator about winning `PoL` slots and their related
    /// info. After this information is provided for a new epoch, the generator
    /// will be able to provide leadership `PoQ` variants.
    fn set_epoch_private(&mut self, new_epoch_private: ProofOfLeadershipQuotaInputs);

    /// Request a new core proof from the prover. It returns `None` if the
    /// maximum core quota has already been reached for this session.
    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof>;
    /// Request a new leadership proof from the prover. It returns `None` if no
    /// secret `PoL` info has been provided for the current epoch.
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

    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs) {
        tracing::info!(target: LOG_TARGET, "Rotating epoch...");
        self.core_proofs_generator.rotate_epoch(new_epoch_public);
        self.leader_proofs_generator = None;
    }

    fn set_epoch_private(&mut self, new_epoch_private: ProofOfLeadershipQuotaInputs) {
        tracing::info!(target: LOG_TARGET, "Setting epoch secret PoL info...");
        if let Some(leader_proofs_generator) = &mut self.leader_proofs_generator {
            leader_proofs_generator.rotate_epoch(
                self.core_proofs_generator.settings.public_inputs.leader,
                new_epoch_private,
            );
        } else {
            self.leader_proofs_generator = Some(RealLeaderProofsGenerator::new(
                self.core_proofs_generator.settings,
                new_epoch_private,
            ));
        }
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        self.core_proofs_generator.get_next_proof().await
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        let Some(leader_proofs_generator) = &mut self.leader_proofs_generator else {
            return None;
        };
        Some(leader_proofs_generator.get_next_proof().await)
    }
}
