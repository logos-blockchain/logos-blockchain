//! `ZkSign` verification benchmarks.
//!
//! Run with `cargo bench -p logos-blockchain-zksign --bench verify`.
//!
//! These measure the payoff of verifying a block's `ZkSign` proofs in a single
//! batch instead of one at a time.

use std::sync::LazyLock;

use divan::counter::ItemsCount;
use lb_groth16::Fr;
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
use logos_blockchain_zksign::{
    ZkSignPrivateKeysData, ZkSignProof, ZkSignVerifierInputs, ZkSignWitnessInputs, batch_verify,
    prove, verify,
};
use num_bigint::BigUint;

/// Batch sizes exercised by the batched and sequential verification
/// benchmarks.
///
/// The largest batch size (1024) is the number of transactions a block can hold
/// because most txs usually contain at least one `Transfer` operation to pay
/// fees.
const BATCH_SIZES: [usize; 6] = [1, 4, 16, 64, 256, 1024];

/// Generate 1024 distinct proofs to be used for all the benchmarks.
static PROOFS: LazyLock<Vec<(ZkSignProof, ZkSignVerifierInputs)>> = LazyLock::new(|| {
    let n_proofs = BATCH_SIZES[BATCH_SIZES.len() - 1];
    (0..n_proofs as u64)
        .map(|index| {
            let secret_keys: [Fr; 32] =
                core::array::from_fn(|key| BigUint::from(index * 32 + key as u64 + 1).into());
            let message_hash = Poseidon2Bn254Hasher::digest(&[BigUint::from(index).into()]);
            prove(ZkSignWitnessInputs::from_witness_data_and_message_hash(
                ZkSignPrivateKeysData::from(secret_keys),
                message_hash,
            ))
            .unwrap()
        })
        .collect()
});

fn main() {
    divan::main();
}

/// Verifies a batch of `batch_size` proofs.
#[divan::bench(args = BATCH_SIZES)]
fn bench_batch_verify(bencher: divan::Bencher, batch_size: usize) {
    let batch = &PROOFS[..batch_size];
    bencher
        .counter(ItemsCount::new(batch_size))
        .bench(|| assert!(divan::black_box(batch_verify(batch)).unwrap()));
}

/// Verifies `batch_size` proofs one at a time, as the baseline
/// [`bench_batch_verify`] is compared against.
#[divan::bench(args = BATCH_SIZES)]
fn bench_sequential_verify(bencher: divan::Bencher, batch_size: usize) {
    let batch = &PROOFS[..batch_size];
    bencher.counter(ItemsCount::new(batch_size)).bench(|| {
        for (proof, inputs) in batch {
            assert!(divan::black_box(verify(proof, inputs)).unwrap());
        }
    });
}
