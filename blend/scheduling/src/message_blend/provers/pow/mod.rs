use core::{num::NonZeroU64, pin::Pin};

use async_trait::async_trait;
use futures::{
    FutureExt as _,
    stream::{self, Stream, StreamExt as _},
};
use lb_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use lb_blend_proofs::{
    quota::{
        VerifiedProofOfQuota,
        inputs::prove::{PrivateInputs, PublicInputs, private::ProofOfWorkQuotaInputs},
        pow::{PowTarget, solve_puzzle},
    },
    selection::VerifiedProofOfSelection,
};
use lb_core::crypto::ZkHash;
use lb_groth16::{AdditiveGroup as _, fr_to_bytes};
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;
use lb_log_targets::blend;
use lb_utils::tokio::{stream::Buffered, task::spawn_blocking};
use rand::rngs::OsRng;
use tokio::time::Instant;

use crate::message_blend::{
    buffer_size,
    provers::{BlendLayerProof, ProofsGeneratorSettings},
};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::proofs::POW;

/// A `PoQ` generator that deals only with proof of work backed proofs.
///
/// Unlike the core and leadership variants, this one is reachable by a node
/// that holds neither stake nor an SDP declaration: its admission right is the
/// puzzle solution, which the generator mines locally. A solution is worth one
/// message, since the spec sets the per-solution quota `Q_W` to `ß_max`.
#[async_trait]
pub trait PowProofsGenerator: Sized {
    /// Instantiate a new generator for the duration of an epoch.
    fn new(settings: ProofsGeneratorSettings) -> Self;
    /// Get the next `PoW` proof.
    async fn get_next_proof(&mut self) -> Option<BlendLayerProof>;
}

pub struct RealPowProofsGenerator {
    pub(super) settings: ProofsGeneratorSettings,
    proofs_stream: Pin<Box<dyn Stream<Item = BlendLayerProof> + Send>>,
}

#[async_trait]
impl PowProofsGenerator for RealPowProofsGenerator {
    fn new(settings: ProofsGeneratorSettings) -> Self {
        Self {
            settings,
            proofs_stream: create_proof_stream(
                settings.public_inputs,
                buffer_size(settings.public_inputs.pow.pow_quota.get() as usize),
            ),
        }
    }

    async fn get_next_proof(&mut self) -> Option<BlendLayerProof> {
        let start = Instant::now();
        let Some(proof) = self.proofs_stream.next().await else {
            tracing::warn!(target: LOG_TARGET, "PoW proof stream ended. No proof is generated.");
            return None;
        };
        tracing::trace!(target: LOG_TARGET, "Generated PoW Blend layer proof with key nullifier {:?} addressed to node at index {:?} in {:?} ms.", hex::encode(fr_to_bytes(&proof.proof_of_quota.key_nullifier())), proof.proof_of_selection.expected_index(self.settings.membership_size), start.elapsed().as_millis());
        Some(proof)
    }
}

fn create_proof_stream(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    buffer_size: usize,
) -> Pin<Box<dyn Stream<Item = BlendLayerProof> + Send>> {
    let difficulty = public_inputs.pow.pow_blend_difficulty;
    // No ticket is below zero, so the puzzle has no solution and there is
    // nothing to mine for.
    if difficulty == PowTarget::ZERO {
        tracing::debug!(target: LOG_TARGET, "Blend PoW difficulty is zero, so no puzzle solution exists. No PoW proof will be generated for this epoch.");
        return Box::pin(stream::empty());
    }

    let per_solution_quota = public_inputs.pow.pow_quota;

    let epoch_nonce = public_inputs.leader.pol_epoch_nonce;
    tracing::debug!(target: LOG_TARGET, "Generating PoW quota proofs with public inputs: {public_inputs:?}.");

    // Each solution yields exactly `per_solution_quota` proofs, indexed
    // `0..per_solution_quota`, and a fresh solution is mined when they run out.
    // The key nullifier is a function of the (nonce, index) pair, so the proofs
    // of one solution get distinct nullifiers, and successive solutions are
    // mined from independently sampled nonces and therefore get distinct
    // nullifiers too. How the stream's proofs map onto messages is the caller's
    // business: a quota below the number of encapsulations in a message simply
    // means a message spans more than one solution.
    Box::pin(Buffered::new(
        solution_stream(epoch_nonce, difficulty).flat_map(move |solution| {
            stream::iter(per_solution_quota.values_range()).map(move |message_release_index| {
                let solution = solution.clone();
                
                let task = spawn_blocking("logos/blend/pow-poq-blocking", move || {
                    let ephemeral_signing_key = UnsecuredEd25519Key::generate_with_blake_rng();
                    let (proof_of_quota, secret_selection_randomness) = VerifiedProofOfQuota::new(
                        &PublicInputs {
                            signing_key: ephemeral_signing_key.public_key().into_inner(),
                            core: public_inputs.core,
                            leader: public_inputs.leader,
                            pow: public_inputs.pow,
                        },
                        PrivateInputs::new_proof_of_work_quota_inputs(
                            message_release_index,
                            solution,
                        ),
                    )
                    .expect("PoW PoQ proof creation should not fail.");
                    let proof_of_selection = VerifiedProofOfSelection::new(secret_selection_randomness);
                    BlendLayerProof {
                        proof_of_quota,
                        proof_of_selection,
                        ephemeral_signing_key,
                    }
                });

                async move {
                    let pow_proof = task.await.expect("Spawning task for PoW proof generation should not fail.");

                    tracing::trace!(target: LOG_TARGET, "Generated PoW PoQ within the stream for message release index {message_release_index:?} with key nullifier {:?} and public key {:?}.", hex::encode(fr_to_bytes(&pow_proof.proof_of_quota.key_nullifier())), pow_proof.ephemeral_signing_key.public_key());
                    pow_proof
                }
            })
        }),
        buffer_size,
    ))
}

/// An endless stream of puzzle solutions.
fn solution_stream(
    epoch_nonce: ZkHash,
    difficulty: PowTarget,
) -> impl Stream<Item = ProofOfWorkQuotaInputs> + Send {
    stream::unfold((), move |()| {
        mine_solution(epoch_nonce, difficulty).map(|solution| Some((solution, ())))
    })
}

/// Number of candidate nonces a single blocking search round tries.
///
/// The search occupies a blocking thread for as long as it runs, so it is
/// broken into rounds: between them the task returns to the runtime, which is
/// what lets a generator that is dropped mid-search stop being mined for. The
/// bound only has to keep a round short relative to an epoch.
const CANDIDATES_PER_SEARCH_ROUND: NonZeroU64 = NonZeroU64::new(1 << 16u8).unwrap();

/// Searches for a puzzle solution, one blocking round at a time, until it finds
/// one.
///
/// The caller must have established that `difficulty` is satisfiable;
/// otherwise this never returns.
async fn mine_solution(epoch_nonce: ZkHash, difficulty: PowTarget) -> ProofOfWorkQuotaInputs {
    let start = Instant::now();
    let mut rounds: u64 = 0;

    loop {
        rounds = rounds.saturating_add(1);
        let round_outcome = spawn_blocking("logos/blend/pow-puzzle-search-round", move || {
            solve_puzzle(
                epoch_nonce,
                difficulty,
                &mut OsRng,
                CANDIDATES_PER_SEARCH_ROUND,
            )
        })
        .await
        .expect("PoW puzzle search round should not fail.");

        if let Some(solution) = round_outcome {
            tracing::trace!(target: LOG_TARGET, "Found a Blend PoW solution after {rounds} round(s) of {CANDIDATES_PER_SEARCH_ROUND} candidates in {} ms.", start.elapsed().as_millis());
            break solution;
        }
    }
}
