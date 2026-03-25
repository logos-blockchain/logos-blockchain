use core::cmp::Ordering;

use async_trait::async_trait;
use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
use lb_blend_proofs::quota::inputs::prove::{
    private::ProofOfLeadershipQuotaInputs, public::LeaderInputs,
};
use lb_cryptarchia_engine::Epoch;

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
    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs, new_epoch: Epoch);
    /// Notify the proof generator about winning `PoL` slots and their related
    /// info. After this information is provided for a new epoch, the generator
    /// will be able to provide leadership `PoQ` variants.
    fn set_epoch_private(
        &mut self,
        new_epoch_private: ProofOfLeadershipQuotaInputs,
        new_epoch_public: LeaderInputs,
        new_epoch: Epoch,
    );
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

    // Changes epoch-related info for the core generator, and stops the old leader
    // generator if it's still on the previous epoch. If not, `rotate_epoch` is
    // effectively a no-op.
    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs, new_epoch: Epoch) {
        match self.core_proofs_generator.current_epoch().cmp(&new_epoch) {
            Ordering::Less => {
                tracing::info!(target: LOG_TARGET, "Rotating epoch...");
                self.core_proofs_generator.rotate_epoch(new_epoch_public);
            }
            Ordering::Equal => {
                tracing::debug!(target: LOG_TARGET, "Core proofs generator already on the new epoch, ignoring the new public epoch info received.");
            }
            Ordering::Greater => {
                panic!(
                    "Public epoch info should never provide an epoch smaller than what the core proofs generator returns as current, as the public epoch info should never lag behind."
                );
            }
        }

        let Some(leader_proofs_generator) = self.leader_proofs_generator.take() else {
            return;
        };

        match leader_proofs_generator.current_epoch().cmp(&new_epoch) {
            Ordering::Less => {
                tracing::debug!(target: LOG_TARGET, "Stopping old epoch leadership proofs generator until new secret PoL info is provided.");
            }
            Ordering::Equal => {
                tracing::debug!(target: LOG_TARGET, "Leadership proofs generator already on the new epoch, ignoring the new public epoch info received.");
                self.leader_proofs_generator = Some(leader_proofs_generator);
            }
            Ordering::Greater => {
                panic!(
                    "Secret PoL info for new epoch should never provide an epoch greater than what the public epoch info returns, as the two should always be yielded together, or at most the secret PoL info would lag behind if the node has no or close to no stake."
                );
            }
        }
    }

    // Creates a new leader proofs generator with the provided public+private
    // secret.
    fn set_epoch_private(
        &mut self,
        new_epoch_private: ProofOfLeadershipQuotaInputs,
        new_epoch_public: LeaderInputs,
        new_epoch: Epoch,
    ) {
        // Update core proof generation and optionally deactivates leadership proof
        // generation, which is then re-created below.
        self.rotate_epoch(new_epoch_public, new_epoch);

        let current_session_local_node_index = self.core_proofs_generator.settings.local_node_index;
        let current_session_membership_size = self.core_proofs_generator.settings.membership_size;
        let current_session_core_public_inputs =
            self.core_proofs_generator.settings.public_inputs.core;
        let current_session = self.core_proofs_generator.settings.public_inputs.session;

        self.leader_proofs_generator = Some(RealLeaderProofsGenerator::new(
            ProofsGeneratorSettings {
                epoch: new_epoch,
                local_node_index: current_session_local_node_index,
                membership_size: current_session_membership_size,
                public_inputs: PoQVerificationInputsMinusSigningKey {
                    core: current_session_core_public_inputs,
                    session: current_session,
                    leader: new_epoch_public,
                },
                encapsulation_layers: self.core_proofs_generator.settings.encapsulation_layers,
            },
            new_epoch_private,
        ));
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        let proof = self.core_proofs_generator.get_next_proof().await?;
        tracing::debug!(target: LOG_TARGET, "Generated core PoQ {:?} with settings: {:?}, epoch: {:?} and signing key: {:?}", proof.proof_of_quota, self.core_proofs_generator.settings, self.core_proofs_generator.settings.epoch, proof.ephemeral_signing_key.public_key());
        Some(proof)
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        let Some(leader_proofs_generator) = &mut self.leader_proofs_generator else {
            return None;
        };
        let proof = leader_proofs_generator.get_next_proof().await;
        tracing::debug!(target: LOG_TARGET, "Generated leadership PoQ {:?} with settings: {:?}, epoch: {:?} and signing key: {:?}", proof.proof_of_quota, leader_proofs_generator.settings, leader_proofs_generator.settings.epoch, proof.ephemeral_signing_key.public_key());
        Some(proof)
    }
}
