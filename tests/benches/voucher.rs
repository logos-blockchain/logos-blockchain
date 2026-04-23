//! Benchmark MMR vs [`DynamicMerkleTree`] for voucher insertion during a single
//! epoch transition.
//!
//! Scenario: A network of 50 nodes proposing blocks in round-robin. Each node
//! tracks its own voucher paths (one per block it proposed). We simulate one
//! full epoch (21,600 insertions) starting from a tree that already has
//! `initial_size` elements. Every 50th insertion adds a new tracked path
//! (our node's vouchers in round-robin).
//!
//! For MMR, each insertion calls `push_with_paths` which eagerly updates all
//! tracked paths.
//! For [`DynamicMerkleTree`], each insertion is just `insert` — paths are
//! computed lazily on demand, so insertion alone is the fair per-block
//! comparison.

use divan::{Bencher, black_box};
use lb_core::{
    crypto::ZkHasher,
    mantle::ops::leader_claim::{VoucherCm, VoucherSecret},
};
use lb_groth16::Fr;
use lb_mmr::MerkleMountainRange;
use lb_utxotree::DynamicMerkleTree;

const SAMPLE_COUNT: u32 = 3;

/// Number of nodes in the network (round-robin block proposal).
const NUM_NODES: u64 = 50;
/// Blocks per epoch.
const BLOCKS_PER_EPOCH: u64 = 21_600;

/// Insert 21,600 vouchers into an empty MMR.
/// 21600/50=432 newly added voucher paths are tracked/updated.
#[divan::bench(sample_count = SAMPLE_COUNT)]
fn mmr_1st_epoch(bencher: Bencher) {
    bench_mmr_epoch(bencher, 0);
}

/// Insert 21,600 vouchers into MMR that already has 10 epochs worth of vouchers
/// (216,000). 21600/60*10=4320 existing tracked vouchers are updated,
/// and 432 newly added voucher paths are tracked/updated.
#[divan::bench(sample_count = SAMPLE_COUNT)]
fn mmr_11th_epoch(bencher: Bencher) {
    bench_mmr_epoch(bencher, BLOCKS_PER_EPOCH * 10);
}

/// Same as [`mmr_1st_epoch`] but no path tracking since Tree keeps all leaves.
#[divan::bench(sample_count = SAMPLE_COUNT)]
fn tree_1st_epoch(bencher: Bencher) {
    bench_tree_epoch(bencher, 0);
}

/// Same as [`mmr_11th_epoch`] but no path tracking since Tree keeps all leaves.
#[divan::bench(sample_count = SAMPLE_COUNT)]
fn tree_11th_epoch(bencher: Bencher) {
    bench_tree_epoch(bencher, BLOCKS_PER_EPOCH * 10);
}

type Mmr = MerkleMountainRange<VoucherCm, ZkHasher>;
type Tree = DynamicMerkleTree<VoucherCm, ZkHasher>;

fn voucher(i: u64) -> VoucherCm {
    VoucherCm::from_secret(VoucherSecret::from(Fr::from(i)))
}

/// Simulate one full epoch of 21,600 insertions into an MMR that already has
/// `initial_size` elements. Every NUM_NODES-th insertion adds a new tracked
/// path (our node's vouchers). Measures total time for the epoch.
fn bench_mmr_epoch(bencher: Bencher, initial_size: u64) {
    let mut mmr = Mmr::new();
    let mut paths: Vec<lb_mmr::MerklePath> = Vec::new();

    for i in 0..initial_size {
        let (new_mmr, new_path) = mmr.push_with_paths(voucher(i), &mut paths).unwrap();
        if i % NUM_NODES == 0 {
            paths.push(new_path);
        }
        mmr = new_mmr;
    }

    bencher.bench_local(|| {
        let mut mmr = mmr.clone();
        let mut paths = paths.clone();

        for i in 0..BLOCKS_PER_EPOCH {
            let (new_mmr, new_path) = mmr
                .push_with_paths(voucher(initial_size + i), &mut paths)
                .unwrap();
            mmr = new_mmr;
            if i % NUM_NODES == 0 {
                paths.push(new_path);
            }
        }

        black_box((&mmr, &paths));
    });
}

/// Same epoch simulation for [`DynamicMerkleTree`]. Insert 21,600 elements.
fn bench_tree_epoch(bencher: Bencher, initial_size: u64) {
    let mut tree = Tree::new();
    for i in 0..initial_size {
        let (new_tree, _) = tree.insert(voucher(i));
        tree = new_tree;
    }

    bencher.bench_local(|| {
        let mut tree = tree.clone();

        for i in 0..BLOCKS_PER_EPOCH {
            let (new_tree, _) = tree.insert(voucher(initial_size + i));
            tree = new_tree;
        }

        black_box(&tree);
    });
}

fn main() {
    divan::main();
}
