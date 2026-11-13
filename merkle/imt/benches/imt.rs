//! Costs of the nullifier tree over `RocksDB`, at the scale the chain asks of
//! it.
//!
//! A tree of `2^LOG_N` finalized nullifiers is imported once per size. The
//! nullifiers of a block fall between imported ones, so their low leaves spread
//! over the whole tree. The `block` bench runs a whole block as the ledger does
//! once it is final: insert, seal, commit and write, finalize, and collect the
//! garbage of the previous base. The other benches time one step each.
//!
//! Run with
//!
//! ```text
//! cargo bench -p logos-blockchain-imt --bench imt [-- <filter>]
//! ```

use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};

use divan::{Bencher, black_box};
use lb_kv_store::{DynKvStore, ErasedKv, KvStore as _, RocksKv};
use lb_poseidon2::{Digest as _, Fr, Poseidon2Bn254Hasher};
use logos_blockchain_imt::{
    LeafIndex, NullifierLeaf,
    store::{BlockId, Committed, Finalized, Imt, LiveBlock, VersionRegistry},
};
use tempfile::TempDir;

type Tree = Imt<DynKvStore, Poseidon2Bn254Hasher, 40>;

fn main() {
    divan::main();
}

/// The nullifiers of a block: from one to the most the Execution Gas limit of
/// a block pays for, at `EXECUTION_NULLIFIER_GAS` each.
const BLOCK_SIZES: &[usize] = &[1, 16, 128, 512, 1024, 1800];
// The imported nullifiers are multiples of `STEP`.
const STEP: u64 = 1 << 40;

struct Fixture {
    _directory: TempDir,
    store: DynKvStore,
    registry: VersionRegistry<DynKvStore>,
    // the finalized tree, advanced by every finalized block
    tree: Mutex<Tree>,
    next_block: AtomicU64,
    seed: AtomicU64,
}

impl Fixture {
    fn new(log_n: u32) -> Self {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store =
            ErasedKv::into_dyn(RocksKv::open(directory.path()).expect("the database opens"));
        let len = 1u64 << log_n;
        let leaves: Vec<NullifierLeaf> = (0..len)
            .map(|index| NullifierLeaf {
                nullifier: Fr::from(index * STEP),
                next_nullifier: Fr::from(if index + 1 == len {
                    0
                } else {
                    (index + 1) * STEP
                }),
            })
            .collect();
        let tree =
            Tree::import(Arc::clone(&store), &leaves, block_id(0)).expect("the tree imports");
        Self {
            _directory: directory,
            registry: VersionRegistry::new(Arc::clone(&store)),
            store,
            tree: Mutex::new(tree),
            next_block: AtomicU64::new(1),
            seed: AtomicU64::new(0x9e37_79b9_7f4a_7c15 ^ u64::from(log_n)),
        }
    }

    fn tree(&self) -> Tree {
        self.tree.lock().expect("not poisoned").clone()
    }

    fn block_id(&self) -> BlockId {
        block_id(self.next_block.fetch_add(1, Ordering::Relaxed))
    }

    // A random value below the largest imported nullifier.
    fn random(&self, below: u64) -> u64 {
        let mut value = self
            .seed
            .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
        value ^= value >> 30;
        value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value ^= value >> 27;
        value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
        (value ^ (value >> 31)) % below
    }

    // `count` nullifiers absent from the tree: none is a multiple of `STEP`.
    fn fresh(&self, count: usize) -> Vec<Fr> {
        let below = self.tree().len() * STEP;
        let mut nullifiers = Vec::with_capacity(count);
        while nullifiers.len() < count {
            let value = self.random(below);
            if !value.is_multiple_of(STEP) {
                nullifiers.push(Fr::from(value));
            }
        }
        nullifiers
    }

    // The base with `nullifiers` inserted, not sealed yet.
    fn inserted(&self, nullifiers: &[Fr]) -> Tree {
        nullifiers.iter().fold(
            self.tree().rebase().expect("the base rebases"),
            |tree, nullifier| tree.insert(*nullifier).expect("a fresh nullifier inserts"),
        )
    }
}

fn block_id(n: u64) -> BlockId {
    let mut id = [0; 32];
    id[24..].copy_from_slice(&n.to_be_bytes());
    BlockId(id)
}

fn fixture<const LOG_N: u32>() -> &'static Fixture {
    static FIXTURES: [OnceLock<Fixture>; 2] = [OnceLock::new(), OnceLock::new()];
    let slot = match LOG_N {
        16 => 0,
        20 => 1,
        _ => unreachable!("a tree size the benches import"),
    };
    FIXTURES[slot].get_or_init(|| Fixture::new(LOG_N))
}

/// Inserting the nullifiers of a block into the finalized base: each finds its
/// low leaf and reads the path of both leaves. No hashing.
#[divan::bench(consts = [16, 20], args = BLOCK_SIZES, sample_count = 10)]
fn insert<const LOG_N: u32>(bencher: Bencher, block_size: usize) {
    let fixture = fixture::<LOG_N>();
    bencher
        .counter(block_size)
        .with_inputs(|| {
            (
                fixture.tree().rebase().expect("rebases"),
                fixture.fresh(block_size),
            )
        })
        .bench_local_values(|(tree, nullifiers)| {
            black_box(nullifiers.iter().fold(tree, |tree, nullifier| {
                tree.insert(*nullifier).expect("a fresh nullifier inserts")
            }))
        });
}

/// Hashing the paths a block changed, once, up to its root.
#[divan::bench(consts = [16, 20], args = BLOCK_SIZES, sample_count = 10)]
fn seal<const LOG_N: u32>(bencher: Bencher, block_size: usize) {
    let fixture = fixture::<LOG_N>();
    bencher
        .counter(block_size)
        .with_inputs(|| fixture.inserted(&fixture.fresh(block_size)))
        .bench_local_values(|tree| {
            let sealed = tree.seal().expect("the block seals");
            black_box(sealed.root().expect("the root reads"))
        });
}

/// Writing a sealed block under a new version: the nodes it changed, their
/// stale records and its index entries, in one `RocksDB` batch.
#[divan::bench(consts = [16, 20], args = BLOCK_SIZES, sample_count = 10)]
fn commit<const LOG_N: u32>(bencher: Bencher, block_size: usize) {
    let fixture = fixture::<LOG_N>();
    bencher
        .counter(block_size)
        .with_inputs(|| {
            (
                fixture
                    .inserted(&fixture.fresh(block_size))
                    .seal()
                    .expect("the block seals"),
                fixture.block_id(),
            )
        })
        .bench_local_values(|(sealed, id)| {
            let allocation = fixture.registry.allocate(id).expect("a version");
            let Committed { imt, writes } = sealed.commit(allocation.version).expect("commits");
            let mut batch = allocation.writes;
            batch.append(writes);
            fixture.store.write_batch(batch).expect("the batch writes");
            black_box(imt)
        });
}

/// A whole block on the canonical chain: insert, seal, commit and write,
/// finalize, and collect the garbage the new base leaves.
#[divan::bench(consts = [16, 20], args = BLOCK_SIZES, sample_count = 10)]
fn block<const LOG_N: u32>(bencher: Bencher, block_size: usize) {
    let fixture = fixture::<LOG_N>();
    bencher
        .counter(block_size)
        .with_inputs(|| (fixture.fresh(block_size), fixture.block_id()))
        .bench_local_values(|(nullifiers, id)| {
            let mut base = fixture.tree.lock().expect("not poisoned");
            let tree = nullifiers.iter().fold(
                base.rebase().expect("the base rebases"),
                |tree, nullifier| tree.insert(*nullifier).expect("a fresh nullifier inserts"),
            );
            let sealed = tree.seal().expect("the block seals");
            let allocation = fixture.registry.allocate(id).expect("a version");
            let Committed { imt, writes } = sealed.commit(allocation.version).expect("commits");
            let mut batch = allocation.writes;
            batch.append(writes);
            fixture.store.write_batch(batch).expect("the batch writes");
            let Finalized { imt, writes } = imt.finalize(id).expect("finalizes");
            let mut batch = writes;
            batch.append(VersionRegistry::<DynKvStore>::release_writes(LiveBlock {
                version: allocation.version,
                block_id: id,
            }));
            fixture.store.write_batch(batch).expect("the batch writes");
            let garbage = Tree::collect_garbage(&fixture.store, imt.len()).expect("collects");
            fixture
                .store
                .write_batch(garbage)
                .expect("the batch writes");
            *base = imt;
        });
}

/// One Poseidon2 compression of two children, of which `seal` hashes one per
/// node a block changed.
#[divan::bench]
fn compress(bencher: Bencher) {
    let fixture = fixture::<16>();
    bencher
        .with_inputs(|| [fixture.fresh(1)[0], fixture.fresh(1)[0]])
        .bench_local_values(|children| black_box(Poseidon2Bn254Hasher::compress(&children)));
}

/// Whether a nullifier is spent, as mempool and block validation ask.
#[divan::bench(consts = [16, 20])]
fn contains<const LOG_N: u32>(bencher: Bencher) {
    let fixture = fixture::<LOG_N>();
    let tree = fixture.tree();
    bencher
        .with_inputs(|| fixture.fresh(1)[0])
        .bench_local_values(|nullifier| black_box(tree.contains(&nullifier).expect("reads")));
}

/// The low leaf of an absent nullifier, with its path: the non-membership
/// witness a wallet proves with.
#[divan::bench(consts = [16, 20])]
fn low_leaf<const LOG_N: u32>(bencher: Bencher) {
    let fixture = fixture::<LOG_N>();
    let tree = fixture.tree();
    bencher
        .with_inputs(|| fixture.fresh(1)[0])
        .bench_local_values(|nullifier| {
            let low = tree
                .low_leaf(&nullifier)
                .expect("reads")
                .expect("a low leaf");
            black_box(tree.path(low.index).expect("reads"))
        });
}

/// The path of a leaf.
#[divan::bench(consts = [16, 20])]
fn path<const LOG_N: u32>(bencher: Bencher) {
    let fixture = fixture::<LOG_N>();
    let tree = fixture.tree();
    bencher
        .with_inputs(|| LeafIndex::new(fixture.random(tree.len())))
        .bench_local_values(|index| black_box(tree.path(index).expect("reads")));
}
