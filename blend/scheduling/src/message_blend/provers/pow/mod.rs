use core::{num::NonZeroU64, pin::Pin};

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt as _};
use lb_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use lb_blend_proofs::{
    quota::{
        Quota, VerifiedProofOfQuota,
        inputs::prove::{PrivateInputs, PublicInputs, private::ProofOfWorkQuotaInputs},
        pow::{PowTarget, solve_puzzle},
    },
    selection::VerifiedProofOfSelection,
};
use lb_core::crypto::ZkHash;
use lb_groth16::{AdditiveGroup as _, fr_to_bytes};
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;
use lb_log_targets::blend;
use lb_utils::tokio::task::spawn_blocking;
use rand::rngs::OsRng;
use tokio::time::Instant;

use crate::message_blend::provers::{BlendLayerProof, ProofsGeneratorSettings};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::proofs::POW;

/// Number of candidate nonces a single blocking search round tries.
///
/// The search occupies a blocking thread for as long as it runs, so it is
/// broken into rounds: between them the task returns to the runtime, which is
/// what lets a generator that is dropped mid-search stop being mined for. The
/// bound only has to keep a round short relative to an epoch — at the spec's
/// reference rate of tens of microseconds per candidate this is seconds of
/// work, and a difficulty that needs more than one round is the normal case.
const CANDIDATES_PER_SEARCH_ROUND: NonZeroU64 = NonZeroU64::new(1 << 16).unwrap();

/// How many proofs to keep in flight, as a multiple of the per-solution quota.
///
/// One quota's worth is what a consumer draws before the next solution has to
/// be mined, so buffering two keeps the following solution's proofs coming
/// while the current one's are handed out.
const BUFFERED_SOLUTIONS: usize = 2;

/// The number of proofs the stream keeps in flight for a given quota.
const fn buffer_size(pow_quota: Quota) -> usize {
    (pow_quota.get() as usize).saturating_mul(BUFFERED_SOLUTIONS)
}

/// A `PoQ` generator that deals only with proof of work backed proofs.
///
/// Unlike the core and leadership variants, this one is reachable by a node
/// that holds neither stake nor an SDP declaration: its admission right is the
/// puzzle solution, which the generator mines locally. A solution is worth one
/// message, since the spec sets the per-solution quota `Q_W` to `ß_max`.
#[async_trait]
pub trait PowProofsGenerator: Sized {
    /// Instantiate a new generator for the duration of an epoch.
    ///
    /// The epoch's `PoW` public inputs — the Blend difficulty the puzzle is
    /// solved against and the per-solution quota — are carried by `settings`.
    fn new(settings: ProofsGeneratorSettings) -> Self;
    /// Get the next proof of work backed proof, mining a fresh solution
    /// whenever the previous one's quota is used up. It returns `None` if the
    /// epoch's `PoW` public inputs admit no proof at all.
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
                buffer_size(settings.public_inputs.pow.pow_quota),
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
    // nothing to mine for. This is checked once here rather than per search
    // round, so that an epoch whose inputs disable the branch costs nothing
    // instead of spinning a blocking thread that can never succeed.
    if difficulty == PowTarget::ZERO {
        tracing::warn!(target: LOG_TARGET, "Blend PoW difficulty is zero, so no puzzle solution exists. No PoW proof will be generated for this epoch.");
        return Box::pin(stream::empty());
    }

    // A quota of zero admits no key index, so no solution can be turned into a
    // proof and the search would run forever without ever yielding one.
    let per_solution_quota = public_inputs.pow.pow_quota;
    if per_solution_quota == Quota::ZERO {
        tracing::warn!(target: LOG_TARGET, "Blend PoW quota is zero, so no solution can be spent. No PoW proof will be generated for this epoch.");
        return Box::pin(stream::empty());
    }

    let epoch_nonce = public_inputs.leader.pol_epoch_nonce;
    tracing::debug!(target: LOG_TARGET, "Generating PoW quota proofs, {per_solution_quota} per solution, with public inputs: {public_inputs:?}.");

    // Each solution yields exactly `per_solution_quota` proofs, indexed
    // `0..per_solution_quota`, and a fresh solution is mined when they run out.
    // The key nullifier is a function of the (nonce, index) pair, so the proofs
    // of one solution get distinct nullifiers, and successive solutions are
    // mined from independently sampled nonces and therefore get distinct
    // nullifiers too. How the stream's proofs map onto messages is the caller's
    // business: a quota below the number of encapsulations in a message simply
    // means a message spans more than one solution.
    //
    // Unlike the core and leadership streams, this one is not pre-polled: a
    // granted quota is going to be spent, whereas the `PoW` branch may never be
    // asked for a proof, and a puzzle search occupies a blocking thread for as
    // long as it runs. Mining therefore starts on the first request and never
    // runs more than one solution ahead of the proofs actually consumed.
    Box::pin(
        solution_stream(epoch_nonce, difficulty).flat_map(move |solution| {
            stream::iter(per_solution_quota.values_range()).map(move |message_release_index| {
                let solution = solution.clone();

                // Spawn eagerly here (outside `async move`) so the blocking task starts as
                // soon as the stream buffer slot is filled, not when the future is first polled.
                // Without this, `spawn_blocking` would only be called when `FuturesOrdered`
                // first polls the future — which only happens when the consumer polls the
                // stream — causing avoidable latency when the consumer is idle.
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
        })
        .buffered(buffer_size),
    )
}

/// An endless stream of puzzle solutions, each mined as the stream is polled.
fn solution_stream(
    epoch_nonce: ZkHash,
    difficulty: PowTarget,
) -> impl Stream<Item = ProofOfWorkQuotaInputs> + Send {
    stream::repeat(()).then(move |()| mine_solution(epoch_nonce, difficulty))
}

/// Searches for a puzzle solution, one blocking round at a time, until it finds
/// one.
///
/// The caller must have established that `difficulty` is satisfiable;
/// otherwise this never returns.
async fn mine_solution(epoch_nonce: ZkHash, difficulty: PowTarget) -> ProofOfWorkQuotaInputs {
    let start = Instant::now();
    for round in 1u64.. {
        let solution = spawn_blocking("logos/blend/pow-puzzle-search-round", move || {
            solve_puzzle(
                epoch_nonce,
                difficulty,
                &mut OsRng,
                CANDIDATES_PER_SEARCH_ROUND,
            )
        })
        .await
        .expect("PoW puzzle search round should not fail.");

        if let Some(solution) = solution {
            tracing::trace!(target: LOG_TARGET, "Found a Blend PoW solution after {round} round(s) of {CANDIDATES_PER_SEARCH_ROUND} candidates in {:?} ms.", start.elapsed().as_millis());
            return solution;
        }
        tracing::trace!(target: LOG_TARGET, "No Blend PoW solution after {round} round(s) of {CANDIDATES_PER_SEARCH_ROUND} candidates ({:?} ms elapsed). Searching on.", start.elapsed().as_millis());
    }
    unreachable!("The search range is unbounded, so the loop only exits with a solution.");
}
