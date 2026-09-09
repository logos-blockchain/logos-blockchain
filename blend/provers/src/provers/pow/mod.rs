use core::{num::NonZeroU64, pin::Pin};
use std::sync::Arc;

use async_trait::async_trait;
use futures::{
    future::join_all,
    stream::{self, Stream, StreamExt as _},
};
use lb_blend_message::crypto::{
    key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
};
use lb_blend_proofs::{
    quota::{
        KeyIndex, Quota, VerifiedProofOfQuota,
        inputs::prove::{PrivateInputs, PublicInputs, private::ProofOfWorkQuotaInputs},
        pow::{PowTarget, solve_puzzle},
    },
    selection::VerifiedProofOfSelection,
};
use lb_core::crypto::ZkHash;
use lb_groth16::{AdditiveGroup as _, fr_to_bytes};
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;
use lb_log_targets::blend;
use lb_utils::tokio::{
    stream::Buffered,
    task::{CancellableHandle, spawn, spawn_blocking},
};
use rand::rngs::OsRng;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::{sync::oneshot, time::Instant};

use crate::provers::{BlendLayerProof, ProofsGeneratorSettings};

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::prover::POW;
const BUFFER_SIZE: usize = 2;

#[must_use]
pub fn new_mining_pool() -> Arc<ThreadPool> {
    Arc::new(
        ThreadPoolBuilder::new()
            .num_threads(BUFFER_SIZE)
            .thread_name(|index| format!("logos/blend/pow-puzzle-search-{index}"))
            .build()
            .expect("Blend PoW puzzle search thread pool should build."),
    )
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
            proofs_stream: create_proof_stream(
                settings.public_inputs,
                Arc::clone(&settings.pow_mining_pool),
            ),
            settings,
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
    thread_pool: Arc<ThreadPool>,
) -> Pin<Box<dyn Stream<Item = BlendLayerProof> + Send>> {
    let difficulty = public_inputs.pow.pow_blend_difficulty;
    // No ticket is below zero, so the puzzle has no solution and there is
    // nothing to mine for.
    if difficulty == PowTarget::ZERO {
        tracing::debug!(target: LOG_TARGET, "Blend PoW difficulty is zero, so no puzzle solution exists. No PoW proof will be generated for this epoch.");
        return Box::pin(stream::empty());
    }

    // A zero quota admits no key index, so a solution buys nothing. Without
    // this the stream would mine solution after solution, map each to an empty
    // run of proofs, and never yield or end, hanging the caller instead of
    // telling it no proof is available.
    let per_solution_quota = public_inputs.pow.pow_quota;
    if per_solution_quota == Quota::ZERO {
        tracing::debug!(target: LOG_TARGET, "Blend PoW quota is zero, so no solution can be spent. No PoW proof will be generated for this epoch.");
        return Box::pin(stream::empty());
    }

    tracing::debug!(target: LOG_TARGET, "Generating PoW quota proofs with public inputs: {public_inputs:?}.");

    // One item of this stream is one solution: its puzzle search, and the
    // `per_solution_quota` proofs that solution buys, indexed
    // `0..per_solution_quota`. The key nullifier is a function of the (nonce,
    // index) pair, so the proofs of one solution get distinct nullifiers, and
    // successive solutions are mined from independently sampled nonces and
    // therefore get distinct nullifiers too. How the proofs map onto messages is
    // the caller's business: a quota below the number of encapsulations in a
    // message simply means a message spans more than one solution.
    Box::pin(
        Buffered::new(
            stream::repeat_with(move || {
                spawn_solution_proofs(public_inputs, per_solution_quota, Arc::clone(&thread_pool))
            }),
            // This case is different than the other proofs.
            // Because the PoW mining happens once for all encapsulations (all key indices), we
            // buffer only one item in advance, so we have all the `indices` proofs ready when the
            // consumer polls the stream. We will probably change the logic also in the
            // other proof generators so that each item of the stream would be the full set of
            // proofs, since they are all required and to be consumed anyway.
            BUFFER_SIZE,
        )
        .flat_map(stream::iter),
    )
}

/// Starts the work one solution is worth: its puzzle search, and the
/// `per_solution_quota` proofs that solution buys.
fn spawn_solution_proofs(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    per_solution_quota: Quota,
    pool: Arc<ThreadPool>,
) -> impl Future<Output = Vec<BlendLayerProof>> + Send {
    let epoch_nonce = public_inputs.leader.pol_epoch_nonce;
    let difficulty = public_inputs.pow.pow_blend_difficulty;

    let task = CancellableHandle::new(spawn("logos/blend/pow-solution-proofs", async move {
        let solution = mine_solution(epoch_nonce, difficulty, &pool).await;

        join_all(
            per_solution_quota
                .values_range()
                .map(move |message_release_index| {
                    spawn_layer_proof(public_inputs, message_release_index, solution.clone())
                }),
        )
        .await
    }));

    async move {
        task.await
            .expect("PoW solution proving task should not fail.")
    }
}

/// Starts the generation of the layer proof at `message_release_index`, backed
/// by `solution`.
fn spawn_layer_proof(
    public_inputs: PoQVerificationInputsMinusSigningKey,
    message_release_index: KeyIndex,
    solution: ProofOfWorkQuotaInputs,
) -> impl Future<Output = BlendLayerProof> + Send {
    let task = CancellableHandle::new(spawn_blocking("logos/blend/pow-poq-blocking", move || {
        let ephemeral_signing_key = UnsecuredEd25519Key::generate_with_chacha_rng();
        let (proof_of_quota, secret_selection_randomness) = VerifiedProofOfQuota::new(
            &PublicInputs {
                signing_key: ephemeral_signing_key.public_key().into_inner(),
                core: public_inputs.core,
                leader: public_inputs.leader,
                pow: public_inputs.pow,
            },
            PrivateInputs::new_proof_of_work_quota_inputs(message_release_index, solution),
        )
        .expect("PoW PoQ proof creation should not fail.");
        let proof_of_selection = VerifiedProofOfSelection::new(secret_selection_randomness);
        let pow_proof = BlendLayerProof {
            proof_of_quota,
            proof_of_selection,
            ephemeral_signing_key,
        };

        tracing::trace!(target: LOG_TARGET, "Generated PoW PoQ within the stream for message release index {message_release_index:?} with key nullifier {:?} and public key {:?}.", hex::encode(fr_to_bytes(&pow_proof.proof_of_quota.key_nullifier())), pow_proof.ephemeral_signing_key.public_key());
        pow_proof
    }));

    async move {
        task.await
            .expect("Spawning task for PoW proof generation should not fail.")
    }
}

/// Searches for a puzzle solution, one round at a time on `pool`, until it
/// finds one.
///
/// The caller must have established that `difficulty` is satisfiable;
/// otherwise this never returns.
async fn mine_solution(
    epoch_nonce: ZkHash,
    difficulty: PowTarget,
    pool: &ThreadPool,
) -> ProofOfWorkQuotaInputs {
    /// Number of candidate nonces a single search round tries.
    ///
    /// The search occupies a pool thread for as long as it runs, so it is
    /// broken into rounds: a round that nobody is waiting for any more is the
    /// last one, which is what lets a generator that is dropped mid-search
    /// stop being mined for. The bound only has to keep a round short relative
    /// to an epoch.
    const CANDIDATES_PER_SEARCH_ROUND: NonZeroU64 = NonZeroU64::new(1 << 16u8).unwrap();

    let start = Instant::now();
    let mut rounds: u64 = 0;

    loop {
        rounds = rounds.saturating_add(1);
        let (round_sender, round_receiver) = oneshot::channel();
        pool.spawn(move || {
            let outcome = solve_puzzle(
                epoch_nonce,
                difficulty,
                &mut OsRng,
                CANDIDATES_PER_SEARCH_ROUND,
            );
            drop(round_sender.send(outcome));
        });
        let round_outcome = round_receiver
            .await
            .expect("PoW puzzle search round should not fail.");

        if let Some(solution) = round_outcome {
            tracing::trace!(target: LOG_TARGET, "Found a Blend PoW solution after {rounds} round(s) of {CANDIDATES_PER_SEARCH_ROUND} candidates in {} ms.", start.elapsed().as_millis());
            break solution;
        }
    }
}
