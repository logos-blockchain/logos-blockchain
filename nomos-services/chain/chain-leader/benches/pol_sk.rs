use blake2::Digest as _;
use chain_leader::pol::{
    SlotSecret,
    merkle::{MerklePolCache, MerklePolSubtree},
};
use cryptarchia_engine::Slot;
use divan::{Bencher, black_box, counter::ItemsCount};
use groth16::{Fr, fr_from_bytes};
use nomos_utils::blake_rng::{Blake2b256, BlakeRng256Seed};

fn main() {
    divan::main();
}

#[divan::bench(sample_count = 1, sample_size = 10, args = [(25, 20), (25, 10), (25, 5)])]
fn precompute_slot_secret(bencher: Bencher, (tree_depth, cache_depth): (usize, usize)) {
    bencher
        .with_inputs(|| {
            let seed = Blake2b256::digest(b"1987");
            let seed_bytes: [u8; 32] = seed.try_into().unwrap();
            let seed = BlakeRng256Seed::from(seed_bytes);
            seed
        })
        .bench_values(|seed| {
            black_box(MerklePolCache::new(
                seed,
                Slot::new(0),
                tree_depth,
                cache_depth,
            ))
        });
}

#[divan::bench(sample_count = 10, sample_size = 10)]
fn compute_non_cached_subtree(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let seed: Fr = fr_from_bytes(b"1987").unwrap();
            seed
        })
        .bench_values(|seed| black_box(MerklePolSubtree::new(seed, 5).merkle_path_for_index(0)));
}

#[divan::bench(sample_count = 10, sample_size = 10)]
fn precompute_leaves(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let seed = Blake2b256::digest(b"1987");
            let seed_bytes: [u8; 32] = seed.try_into().unwrap();
            let seed = BlakeRng256Seed::from(seed_bytes);
            seed
        })
        .bench_values(|seed| {
            black_box(
                MerklePolCache::leaves_from_seed(seed)
                    .take(2usize.pow(20))
                    .collect::<Vec<_>>(),
            )
        });
}
