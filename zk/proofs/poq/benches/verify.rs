//! Proof of Quota verification benchmarks.
//!
//! Run with `cargo bench -p logos-blockchain-poq --bench verify`.
//!
//! Proving is orders of magnitude slower than verifying, so the proofs are
//! generated once up front and shared by every benchmark below. Expect the
//! first benchmark to sit idle for a while as the pool is built.

use std::sync::LazyLock;

use divan::counter::ItemsCount;
use logos_blockchain_poq::{PoQProof, PoQVerifierInput, batch_verify, prove, verify};

mod common;

/// Batch sizes exercised by the batched and sequential verification
/// benchmarks.
const BATCH_SIZES: [usize; 4] = [1, 4, 16, 32];

/// Number of distinct core node proofs to generate, so that no batch has to
/// repeat a proof.
const PROOF_POOL_SIZE: usize = BATCH_SIZES[BATCH_SIZES.len() - 1];

/// Distinct core node proofs, one per key index. Different indices yield
/// different key nullifiers, which keeps the batches representative of what a
/// node actually verifies.
static CORE_PROOFS: LazyLock<Vec<(PoQProof, PoQVerifierInput)>> = LazyLock::new(|| {
    (0..PROOF_POOL_SIZE as u64)
        .map(|key_index| prove(common::core_node_inputs(key_index)).unwrap())
        .collect()
});

static LEADER_PROOF: LazyLock<(PoQProof, PoQVerifierInput)> =
    LazyLock::new(|| prove(common::leader_inputs(6)).unwrap());

fn main() {
    divan::main();
}

fn bench_single(bencher: divan::Bencher, (proof, inputs): &(PoQProof, PoQVerifierInput)) {
    bencher
        .counter(ItemsCount::new(1usize))
        .with_inputs(|| inputs.clone())
        // The invalid path short circuits, so assert we are timing a successful
        // verification and not a rejection.
        .bench_values(|inputs| assert!(divan::black_box(verify(proof, inputs)).unwrap()));
}

#[divan::bench]
fn bench_verify_core_node(bencher: divan::Bencher) {
    bench_single(bencher, &CORE_PROOFS[0]);
}

#[divan::bench]
fn bench_verify_leader(bencher: divan::Bencher) {
    bench_single(bencher, &LEADER_PROOF);
}

/// Verifies a batch of proofs in a single pairing check.
#[divan::bench(args = BATCH_SIZES)]
fn bench_batch_verify_core_node(bencher: divan::Bencher, batch_size: usize) {
    let batch = &CORE_PROOFS[..batch_size];
    bencher
        .counter(ItemsCount::new(batch_size))
        .bench(|| assert!(divan::black_box(batch_verify(batch)).unwrap()));
}

/// Verifies the same batch one proof at a time, as the baseline
/// [`bench_batch_verify_core_node`] is compared against.
#[divan::bench(args = BATCH_SIZES)]
fn bench_sequential_verify_core_node(bencher: divan::Bencher, batch_size: usize) {
    let batch = &CORE_PROOFS[..batch_size];
    bencher
        .counter(ItemsCount::new(batch_size))
        .with_inputs(|| {
            batch
                .iter()
                .map(|(_, inputs)| inputs.clone())
                .collect::<Vec<_>>()
        })
        .bench_values(|all_inputs| {
            for ((proof, _), inputs) in batch.iter().zip(all_inputs) {
                assert!(divan::black_box(verify(proof, inputs)).unwrap());
            }
        });
}
