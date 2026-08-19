use async_trait::async_trait;
use lb_log_targets::blend;

use crate::message_blend::provers::{
    BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
    leader::{LeaderProofsGenerator as _, RealLeaderProofsGenerator},
    pow::{PowProofsGenerator as _, RealPowProofsGenerator},
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::proofs::LEADER_AND_POW;

/// Proof generator for the two `PoQ` variants an edge node can reach.
///
/// An edge node holds no core quota, so it covers leadership — which needs
/// stake — and proof of work, which needs neither stake nor an SDP declaration
/// and is therefore what a node with no stake at all is left with. The variants
/// are indistinguishable to a verifier, so which one backs a given message is a
/// local decision.
#[async_trait]
pub trait LeaderAndPowProofsGenerator: Sized {
    /// Instantiate a new generator for the duration of an epoch.
    fn new(
        settings: ProofsGeneratorSettings,
        winning_pol_info_stream: WinningPolInfoStream,
    ) -> Self;
    /// Request a new leadership proof from the prover. It returns `None` if all
    /// the winning slots for the current epoch have been used up.
    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof>;
    /// Request a new proof of work backed proof from the prover. It returns
    /// `None` if the epoch's `PoW` public inputs admit no proof at all.
    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof>;
}

/// The generator an edge node runs for the duration of an epoch.
///
/// Unlike the core generator, this one needs no way to be told to stop: an edge
/// node replaces its whole message handler when an epoch rotates, and dropping
/// the generator with it is what abandons the mining stream it owns.
pub struct RealLeaderAndPowProofsGenerator {
    leader_proofs_generator: RealLeaderProofsGenerator,
    pow_proofs_generator: RealPowProofsGenerator,
}

#[async_trait]
impl LeaderAndPowProofsGenerator for RealLeaderAndPowProofsGenerator {
    fn new(
        settings: ProofsGeneratorSettings,
        winning_pol_info_stream: WinningPolInfoStream,
    ) -> Self {
        Self {
            leader_proofs_generator: RealLeaderProofsGenerator::new(
                settings,
                winning_pol_info_stream,
            ),
            pow_proofs_generator: RealPowProofsGenerator::new(settings),
        }
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        self.leader_proofs_generator.get_next_proof().await
    }

    async fn get_next_pow_proof(&mut self) -> Option<BlendLayerProof> {
        let proof = self.pow_proofs_generator.get_next_proof().await?;
        tracing::trace!(
            target: LOG_TARGET,
            key_nullifier = ?proof.proof_of_quota.key_nullifier(),
            signing_key = ?proof.ephemeral_signing_key.public_key(),
            "generated PoW PoQ"
        );
        Some(proof)
    }
}
