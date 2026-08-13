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
use lb_groth16::{AdditiveGroup as _, fr_from_mod_bytes, fr_to_bytes};
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;
use lb_log_targets::blend;
use lb_utils::tokio::task::spawn_blocking;
use rand::{RngCore as _, rngs::OsRng};
use tokio::time::Instant;

use crate::message_blend::{
    buffer_size,
    provers::{BlendLayerProof, ProofsGeneratorSettings},
};

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
                settings.encapsulation_layers,
                buffer_size(settings.encapsulation_layers.get() as usize),
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
    proofs_per_solution: NonZeroU64,
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

    // One solution has to cover a whole message, so the circuit's per-solution
    // quota must admit an index for each of the message's encapsulations. If it
    // does not, no amount of mining helps: the proofs would be rejected by
    // every verifier, so none are generated.
    let Some(per_solution_quota) =
        quota_for_one_message(public_inputs.pow.pow_quota, proofs_per_solution)
    else {
        tracing::error!(target: LOG_TARGET, "Blend PoW quota {} is smaller than the {proofs_per_solution} encapsulations of a single message. No PoW proof will be generated for this epoch.", public_inputs.pow.pow_quota);
        return Box::pin(stream::empty());
    };

    let epoch_nonce = public_inputs.leader.pol_epoch_nonce;
    tracing::debug!(target: LOG_TARGET, "Generating PoW quota proofs, {per_solution_quota} per solution, with public inputs: {public_inputs:?}.");

    // Each solution yields exactly `per_solution_quota` proofs (one original
    // data message's worth of encapsulations), indexed `0..per_solution_quota`.
    // The key nullifier is a function of the (nonce, message index) pair, so
    // the proofs of one solution get distinct nullifiers, and consecutive
    // messages are mined against distinct nonces and therefore get distinct
    // nullifiers too.
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

/// The number of keys one solution must cover, or [`None`] if the epoch's
/// per-solution quota cannot cover a whole message.
fn quota_for_one_message(
    pow_quota: Quota,
    encapsulations_per_message: NonZeroU64,
) -> Option<Quota> {
    let needed = Quota::try_new(encapsulations_per_message.get()).ok()?;
    (needed <= pow_quota).then_some(needed)
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
        // Each round restarts from a freshly sampled nonce rather than
        // continuing the previous one, so that a solution never reveals how
        // long the search took.
        let starting_nonce = random_nonce();
        let solution = spawn_blocking("logos/blend/pow-puzzle-search-round", move || {
            solve_puzzle(
                epoch_nonce,
                difficulty,
                starting_nonce,
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

/// A uniformly sampled starting point for a puzzle search.
fn random_nonce() -> ZkHash {
    let mut bytes = [0u8; size_of::<ZkHash>()];
    OsRng.fill_bytes(&mut bytes);
    fr_from_mod_bytes(&bytes)
}
