use std::collections::BTreeMap;

use ark_ff::Field as _;
use lb_kv_store::{Bytes, KvEntry, MemKv};
use proptest::prelude::*;

use super::*;
use crate::IndexedMerkleTree;

type ZkHasher = lb_poseidon2::Poseidon2Bn254Hasher;
type Store = Arc<MemKv>;
type Tree = Imt<Store, ZkHasher, 40>;
type Oracle = IndexedMerkleTree<ZkHasher, 40>;

fn fr(n: u64) -> Fr {
    Fr::from(n)
}

fn block_id(n: u64) -> BlockId {
    let mut id = [0; 32];
    id[24..].copy_from_slice(&n.to_be_bytes());
    BlockId(id)
}

const GENESIS: u64 = 0;

#[derive(Clone)]
struct Block {
    parent: Option<u64>,
    imt: Tree,
    oracle: Oracle,
    version: Option<Version>,
    nullifiers: Vec<Fr>,
}

/// A state kept to be read after the LIB passed it, like the epoch snapshot of
/// the note sets.
#[derive(Clone)]
struct Pin {
    imt: Tree,
    oracle: Oracle,
}

/// Blocks on forks over one store, each with the oracle of its state. It
/// applies the writes as the ledger is meant to: commit, then finalize the LIB
/// together with the discarding of the pruned forks, then collect the garbage
/// up to the oldest pinned state.
struct Sim {
    store: Store,
    registry: VersionRegistry<Store>,
    blocks: BTreeMap<u64, Block>,
    // the parent of every block ever built
    parents: BTreeMap<u64, Option<u64>>,
    pins: BTreeMap<u64, Pin>,
    lib: u64,
    // the length the garbage was last collected up to
    floor: u64,
    next_id: u64,
}

impl Sim {
    fn new() -> Self {
        let store = Arc::new(MemKv::new());
        let imt = Tree::initialize(Arc::clone(&store), block_id(GENESIS)).unwrap();
        let mut blocks = BTreeMap::new();
        blocks.insert(
            GENESIS,
            Block {
                parent: None,
                imt,
                oracle: Oracle::new(),
                version: None,
                nullifiers: Vec::new(),
            },
        );
        Self {
            registry: VersionRegistry::new(Arc::clone(&store)),
            store,
            blocks,
            parents: [(GENESIS, None)].into(),
            pins: BTreeMap::new(),
            lib: GENESIS,
            floor: 1,
            next_id: 1,
        }
    }

    fn live(&self) -> Vec<u64> {
        self.blocks.keys().copied().collect()
    }

    fn pick(&self, seed: u64) -> u64 {
        let live = self.live();
        live[seed as usize % live.len()]
    }

    fn ancestors(&self, mut id: u64) -> Vec<u64> {
        let mut path = vec![id];
        while let Some(Some(parent)) = self.parents.get(&id) {
            path.push(*parent);
            id = *parent;
        }
        path
    }

    fn descends_from(&self, id: u64, ancestor: u64) -> bool {
        self.ancestors(id).contains(&ancestor)
    }

    // Builds a block on `parent` from its state, as `prepare_update` and
    // `commit_update` would.
    fn build(&self, id: u64, parent: u64, nullifiers: &[Fr]) -> (Block, WriteBatch) {
        let parent_block = &self.blocks[&parent];
        let mut imt = parent_block.imt.rebase().unwrap();
        let mut oracle = parent_block.oracle.clone();
        let mut inserted = Vec::new();
        for nullifier in nullifiers {
            if *nullifier == Fr::ZERO || oracle.contains(nullifier) {
                let expected = if *nullifier == Fr::ZERO {
                    ImtError::ZeroNullifier
                } else {
                    ImtError::AlreadyPresent(*nullifier)
                };
                assert_eq!(imt.insert(*nullifier).unwrap_err(), expected);
                continue;
            }
            imt = imt.insert(*nullifier).unwrap();
            oracle = oracle.insert(*nullifier).unwrap();
            inserted.push(*nullifier);
        }
        // reads before the seal hash on the fly
        assert_eq!(imt.root().unwrap(), oracle.root());
        let imt = imt.seal().unwrap();
        let allocation = self.registry.allocate(block_id(id)).unwrap();
        let Committed { imt, writes } = imt.commit(allocation.version).unwrap();
        let mut batch = allocation.writes;
        batch.append(writes);
        let block = Block {
            parent: Some(parent),
            imt,
            oracle: oracle.flush(),
            version: Some(allocation.version),
            nullifiers: inserted,
        };
        (block, batch)
    }

    fn extend(&mut self, parent: u64, nullifiers: &[Fr]) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let (block, batch) = self.build(id, parent, nullifiers);
        self.store.write_batch(batch).unwrap();
        self.parents.insert(id, Some(parent));
        self.blocks.insert(id, block);
        id
    }

    fn pin(&mut self, id: u64) {
        let block = &self.blocks[&id];
        self.pins.insert(
            id,
            Pin {
                imt: block.imt.clone(),
                oracle: block.oracle.clone(),
            },
        );
    }

    fn unpin(&mut self, id: u64) {
        self.pins.remove(&id);
        self.collect_garbage();
    }

    // Collects the garbage up to the oldest state still read: the LIB, or a
    // pinned state below it.
    fn collect_garbage(&mut self) {
        let lib_len = self.blocks[&self.lib].imt.len();
        let floor = self
            .pins
            .values()
            .map(|pin| pin.imt.len())
            .chain([lib_len])
            .min()
            .unwrap();
        let batch = Tree::collect_garbage(&self.store, floor).unwrap();
        self.store.write_batch(batch).unwrap();
        self.floor = floor;
    }

    // Advances the LIB to `new_lib`, pruning every fork that does not descend
    // from it, in one batch, then collects the garbage.
    fn advance_lib(&mut self, new_lib: u64) {
        assert!(self.descends_from(new_lib, self.lib));
        if new_lib == self.lib {
            return;
        }
        let canonical: Vec<u64> = self
            .ancestors(new_lib)
            .into_iter()
            .take_while(|id| *id != self.lib)
            .collect();
        let Finalized { imt, writes } = self.blocks[&new_lib]
            .imt
            .finalize(block_id(new_lib))
            .unwrap();
        let mut batch = writes;
        for id in &canonical {
            let version = self.blocks[id].version.unwrap();
            batch.append(VersionRegistry::<Store>::release_writes(LiveBlock {
                version,
                block_id: block_id(*id),
            }));
        }
        let stale: Vec<u64> = self
            .live()
            .into_iter()
            .filter(|id| {
                !self.descends_from(*id, new_lib) && !canonical.contains(id) && *id != self.lib
            })
            .collect();
        for id in &stale {
            let version = self.blocks[id].version.unwrap();
            batch.append(Tree::discard_writes(version)).append(
                VersionRegistry::<Store>::release_writes(LiveBlock {
                    version,
                    block_id: block_id(*id),
                }),
            );
        }
        self.store.write_batch(batch).unwrap();
        for id in stale.iter().chain(&canonical).chain([&self.lib]) {
            if *id != new_lib {
                self.blocks.remove(id);
            }
        }
        self.blocks.get_mut(&new_lib).unwrap().imt = imt;
        self.lib = new_lib;
        // the pins off the canonical chain went with their forks
        let pins = std::mem::take(&mut self.pins);
        self.pins = pins
            .into_iter()
            .filter(|(id, _)| self.descends_from(new_lib, *id) || self.descends_from(*id, new_lib))
            .collect();
        self.collect_garbage();
    }

    // Drops every state, reopens the LIB from the store and replays the blocks
    // above it, except those on the forks of `lost`, which the restart does not
    // see. The pins below the LIB come back from their snapshots.
    fn restart(&mut self, lost: &BTreeSet<u64>) {
        let old = std::mem::take(&mut self.blocks);
        let lib = Tree::open(Arc::clone(&self.store)).unwrap();
        assert_eq!(
            Tree::base_meta(&self.store).unwrap().block_id,
            block_id(self.lib)
        );
        lib.check_base().unwrap();
        self.blocks.insert(
            self.lib,
            Block {
                imt: lib,
                ..old[&self.lib].clone()
            },
        );
        // ids grow with the blocks, so a parent is replayed before its children
        let mut replayed = BTreeSet::new();
        for (id, block) in &old {
            let on_lost_fork = self
                .ancestors(*id)
                .iter()
                .any(|ancestor| lost.contains(ancestor));
            if *id == self.lib || on_lost_fork {
                continue;
            }
            let (rebuilt, batch) = self.build(*id, block.parent.unwrap(), &block.nullifiers);
            assert_eq!(rebuilt.version, block.version, "the version is reused");
            assert_eq!(rebuilt.imt.root().unwrap(), block.imt.root().unwrap());
            self.store.write_batch(batch).unwrap();
            self.blocks.insert(*id, rebuilt);
            replayed.insert(*id);
        }
        // the versions the replay did not reuse are of lost forks
        let mut batch = WriteBatch::new();
        for live in self.registry.live_blocks().unwrap() {
            if !replayed.iter().any(|id| block_id(*id) == live.block_id) {
                batch
                    .append(Tree::discard_writes(live.version))
                    .append(VersionRegistry::<Store>::release_writes(live));
            }
        }
        self.store.write_batch(batch).unwrap();
        let pins = std::mem::take(&mut self.pins);
        for (id, pin) in pins {
            let imt = if let Some(block) = self.blocks.get(&id) {
                block.imt.clone()
            } else if self.descends_from(self.lib, id) {
                Tree::from_snapshot(Arc::clone(&self.store), pin.imt.snapshot().unwrap()).unwrap()
            } else {
                continue;
            };
            self.pins.insert(id, Pin { imt, ..pin });
        }
    }

    fn check_state(name: &str, imt: &Tree, oracle: &Oracle, probes: &[Fr]) {
        assert_eq!(imt.len(), oracle.len() as u64, "len of {name}");
        assert_eq!(imt.root().unwrap(), oracle.root(), "root of {name}");
        for probe in probes {
            assert_eq!(
                imt.contains(probe).unwrap(),
                oracle.contains(probe),
                "contains {probe} in {name}"
            );
            let expected = oracle.low_leaf(probe).map(|(index, leaf)| LowLeaf {
                index: LeafIndex::new(index),
                leaf: *leaf,
            });
            assert_eq!(
                imt.low_leaf(probe).unwrap(),
                expected,
                "low leaf of {probe} in {name}"
            );
        }
        let len = imt.len();
        let mut indices: BTreeSet<u64> = [0, len - 1, len / 2].into();
        indices.extend(
            probes
                .iter()
                .filter_map(|probe| oracle.low_leaf(probe).map(|(index, _)| index)),
        );
        for index in indices {
            assert_eq!(
                imt.leaf(LeafIndex::new(index)).unwrap().as_ref(),
                oracle.leaf(index),
                "leaf {index} of {name}"
            );
            assert_eq!(
                imt.path(LeafIndex::new(index))
                    .unwrap()
                    .as_ref()
                    .map(path_nodes),
                oracle.path(index).as_ref().map(path_nodes),
                "path {index} of {name}"
            );
        }
        assert_eq!(imt.leaf(LeafIndex::new(len)).unwrap(), None);
        assert!(imt.path(LeafIndex::new(len)).unwrap().is_none());
    }

    fn check(&self, probes: &[Fr]) {
        for (id, block) in &self.blocks {
            Self::check_state(&format!("block {id}"), &block.imt, &block.oracle, probes);
        }
        for (id, pin) in &self.pins {
            Self::check_state(&format!("pin {id}"), &pin.imt, &pin.oracle, probes);
        }
        self.check_storage();
    }

    // No reachable node is missing, and an unreachable one waits for the
    // garbage collection, which leaves none once nothing waits. The stale
    // records are those of the live blocks above the LIB or of the finalized
    // ones waiting for the collection, the registered versions those of the
    // live blocks, and the base index holds the nullifiers of the LIB.
    fn check_storage(&self) {
        let mut reachable = BTreeSet::new();
        let states = self
            .blocks
            .values()
            .map(|block| &block.imt)
            .chain(self.pins.values().map(|pin| &pin.imt));
        for imt in states {
            let root = imt.committed_root().unwrap();
            collect_reachable(&self.store, NodePosition::root(40), root, &mut reachable);
        }
        let stored: BTreeSet<Vec<u8>> = keys_with_tag(&self.store, NodeKey::TAG);
        let reachable: BTreeSet<Vec<u8>> = reachable.iter().map(StoreKey::encode).collect();
        assert!(reachable.is_subset(&stored), "reachable nodes are stored");

        let stale: Vec<StaleKey> = keys_with_tag(&self.store, StaleKey::TAG)
            .iter()
            .map(|key| StaleKey::decode(key).unwrap())
            .collect();
        let waiting: BTreeSet<Vec<u8>> = stale.iter().map(|stale| stale.node.encode()).collect();
        for unreachable in stored.difference(&reachable) {
            assert!(
                waiting.contains(unreachable),
                "an unreachable node waits for the collection"
            );
        }
        let gc: Vec<GcKey> = keys_with_tag(&self.store, GcKey::TAG)
            .iter()
            .map(|key| GcKey::decode(key).unwrap())
            .collect();
        for entry in &gc {
            assert!(entry.end_len > self.floor, "collected up to the floor");
        }
        let live_versions: BTreeSet<Version> = self
            .blocks
            .iter()
            .filter(|(id, _)| **id != self.lib)
            .map(|(_, block)| block.version.unwrap())
            .collect();
        let gc_versions: BTreeSet<Version> = gc.iter().map(|entry| entry.version).collect();
        for stale in &stale {
            assert!(
                live_versions.contains(&stale.replaced_by)
                    || gc_versions.contains(&stale.replaced_by),
                "stale record of a live block or of one waiting for the collection"
            );
        }
        if gc.is_empty() {
            let unreachable_waiting = stored
                .difference(&reachable)
                .filter(|node| {
                    stale.iter().any(|stale| {
                        !live_versions.contains(&stale.replaced_by) && stale.node.encode() == **node
                    })
                })
                .count();
            assert_eq!(
                unreachable_waiting, 0,
                "with nothing waiting, only nodes replaced above the LIB are unreachable"
            );
        }
        let registered: BTreeSet<Version> = self
            .registry
            .live_blocks()
            .unwrap()
            .into_iter()
            .map(|live| live.version)
            .collect();
        assert_eq!(
            registered, live_versions,
            "registered versions are the live ones"
        );
        assert_eq!(
            keys_with_tag(&self.store, NullifierKey::TAG).len() as u64,
            self.blocks[&self.lib].imt.len(),
            "the base index holds the LIB nullifiers"
        );
    }

    // The store holds exactly the nodes the live and pinned states reach.
    fn assert_no_garbage(&self) {
        let mut reachable = BTreeSet::new();
        let states = self
            .blocks
            .values()
            .map(|block| &block.imt)
            .chain(self.pins.values().map(|pin| &pin.imt));
        for imt in states {
            let root = imt.committed_root().unwrap();
            collect_reachable(&self.store, NodePosition::root(40), root, &mut reachable);
        }
        let reachable: BTreeSet<Vec<u8>> = reachable.iter().map(StoreKey::encode).collect();
        assert_eq!(keys_with_tag(&self.store, NodeKey::TAG), reachable);
    }
}

fn keys_with_tag(store: &MemKv, tag: &[u8]) -> BTreeSet<Vec<u8>> {
    store
        .scan_prefix(tag)
        .unwrap()
        .into_iter()
        .map(|entry| entry.key.to_vec())
        .collect()
}

fn collect_reachable(
    store: &MemKv,
    position: NodePosition,
    child: Child,
    reachable: &mut BTreeSet<NodeKey>,
) {
    let Some(version) = child.version else {
        return;
    };
    let key = NodeKey { version, position };
    if !reachable.insert(key) {
        return;
    }
    let bytes = store
        .get(&key.encode())
        .unwrap()
        .expect("reachable node is stored");
    if let StoredNode::Internal { left, right } = StoredNode::decode(&bytes).unwrap() {
        collect_reachable(store, position.left(), left, reachable);
        collect_reachable(store, position.right(), right, reachable);
    }
}

// `MerkleNode` has no `PartialEq`, so the paths compare as (side, sibling)
fn path_nodes(path: &MerklePath<Fr, 40>) -> Vec<(bool, Fr)> {
    path.iter()
        .map(|node| (matches!(node, MerkleNode::Left(_)), *node.item()))
        .collect()
}

fn frs(values: &[u64]) -> Vec<Fr> {
    values.iter().copied().map(fr).collect()
}

#[test]
fn test_genesis_is_the_sentinel_only() {
    let sim = Sim::new();
    sim.check(&frs(&[1, 5]));
    sim.assert_no_garbage();
    let genesis = &sim.blocks[&GENESIS].imt;
    assert!(genesis.is_finalized());
    assert_eq!(genesis.len(), 1);
    assert_eq!(
        Tree::initialize(Arc::clone(&sim.store), block_id(GENESIS)).unwrap_err(),
        ImtError::AlreadyInitialized
    );
}

#[test]
fn test_single_chain_matches_oracle() {
    let mut sim = Sim::new();
    let mut tip = GENESIS;
    for block in 0..6u64 {
        let nullifiers: Vec<u64> = (0..5).map(|i| (block * 37 + i * 101) % 500 + 1).collect();
        tip = sim.extend(tip, &frs(&nullifiers));
    }
    sim.check(&frs(&[1, 2, 250, 499, 501, 10_000]));
    sim.advance_lib(tip);
    sim.check(&frs(&[1, 2, 250, 499, 501, 10_000]));
    sim.assert_no_garbage();
}

#[test]
fn test_siblings_sharing_a_low_leaf_stay_independent() {
    let mut sim = Sim::new();
    let parent = sim.extend(GENESIS, &frs(&[10, 30]));
    // both insert above 10 and below 30: the same low leaf, rewritten twice
    let a = sim.extend(parent, &frs(&[20]));
    let b = sim.extend(parent, &frs(&[25]));
    let roots = [a, b].map(|id| sim.blocks[&id].imt.root().unwrap());
    assert_ne!(roots[0], roots[1]);
    let probes = frs(&[15, 20, 22, 25, 27, 40]);
    sim.check(&probes);
    // finalizing one fork discards the other
    sim.advance_lib(a);
    assert!(!sim.blocks.contains_key(&b));
    sim.check(&probes);
    sim.assert_no_garbage();
    let child = sim.extend(a, &frs(&[22, 27]));
    sim.check(&probes);
    sim.advance_lib(child);
    sim.check(&probes);
    sim.assert_no_garbage();
}

#[test]
fn test_fork_at_the_lib_is_kept_and_below_is_pruned() {
    let mut sim = Sim::new();
    let b1 = sim.extend(GENESIS, &frs(&[5]));
    let b2 = sim.extend(b1, &frs(&[6]));
    let b3 = sim.extend(b2, &frs(&[7]));
    let below = sim.extend(b1, &frs(&[8])); // diverges below b2
    let below_child = sim.extend(below, &frs(&[9]));
    let at = sim.extend(b2, &frs(&[10])); // diverges at b2
    sim.advance_lib(b2);
    assert!(sim.blocks.contains_key(&b3) && sim.blocks.contains_key(&at));
    assert!(!sim.blocks.contains_key(&below) && !sim.blocks.contains_key(&below_child));
    sim.check(&frs(&[6, 7, 8, 9, 10, 11]));
}

#[test]
fn test_empty_blocks_write_nothing() {
    let mut sim = Sim::new();
    let before = sim.store.len();
    let empty = sim.extend(GENESIS, &[]);
    // only the version registry entries
    assert_eq!(sim.store.len(), before + 3);
    let child = sim.extend(empty, &frs(&[3]));
    sim.check(&frs(&[1, 3, 4]));
    sim.advance_lib(child);
    sim.check(&frs(&[1, 3, 4]));
    sim.assert_no_garbage();
}

#[test]
fn test_reading_a_discarded_state_fails_with_missing_node() {
    let mut sim = Sim::new();
    let keep = sim.extend(GENESIS, &frs(&[10]));
    let doomed = sim.extend(GENESIS, &frs(&[20]));
    let clone = sim.blocks[&doomed].imt.clone();
    sim.advance_lib(keep);
    assert!(matches!(
        clone.path(LeafIndex::new(1)),
        Err(ImtError::MissingNode(_))
    ));
}

#[test]
fn test_rebase_frees_the_finalized_overlay() {
    let mut sim = Sim::new();
    let a = sim.extend(GENESIS, &frs(&[1, 2, 3]));
    let b = sim.extend(a, &frs(&[4]));
    assert_eq!(sim.blocks[&b].imt.unfinalized_versions().count(), 2);
    sim.advance_lib(a);
    let rebased = sim.blocks[&b].imt.rebase().unwrap();
    assert_eq!(rebased.unfinalized_versions().count(), 1);
    assert_eq!(rebased.index.size(), 1);
    assert_eq!(rebased.root().unwrap(), sim.blocks[&b].imt.root().unwrap());
    for nullifier in frs(&[1, 2, 3, 4]) {
        assert!(rebased.contains(&nullifier).unwrap());
    }
}

#[test]
fn test_pinned_snapshot_reads_below_the_lib() {
    let mut sim = Sim::new();
    let aged = sim.extend(GENESIS, &frs(&[10, 30]));
    sim.pin(aged);
    // later blocks rewrite the low leaf of 20 and insert around it
    let a = sim.extend(aged, &frs(&[20, 25, 15]));
    let b = sim.extend(a, &frs(&[22, 40]));
    sim.advance_lib(b);
    let probes = frs(&[12, 15, 20, 22, 26, 35, 45]);
    // the snapshot still reads its own tree, not the newer base index
    sim.check(&probes);
    let pin = &sim.pins[&aged].imt;
    assert!(!pin.contains(&fr(20)).unwrap());
    assert_eq!(
        pin.low_leaf(&fr(20)).unwrap().unwrap().leaf.next_nullifier,
        fr(30)
    );
    // the two blocks above the snapshot wait for it
    assert_eq!(sim.store.scan_prefix(&GcKey::prefix()).unwrap().len(), 2);
    // released, the snapshot no longer keeps the replaced nodes
    sim.unpin(aged);
    sim.check(&probes);
    sim.assert_no_garbage();
}

#[test]
fn test_snapshot_survives_restart() {
    let mut sim = Sim::new();
    let aged = sim.extend(GENESIS, &frs(&[5, 50]));
    sim.pin(aged);
    let a = sim.extend(aged, &frs(&[7, 49]));
    sim.advance_lib(a);
    let b = sim.extend(a, &frs(&[6]));
    sim.restart(&BTreeSet::new());
    let probes = frs(&[6, 7, 8, 49, 51]);
    sim.check(&probes);
    assert!(sim.blocks.contains_key(&b));
    assert_eq!(
        Tree::from_snapshot(
            Arc::clone(&sim.store),
            sim.blocks[&b].imt.snapshot().unwrap()
        )
        .unwrap_err(),
        ImtError::BaseMismatch,
        "a state above the base is not a snapshot"
    );
}

#[test]
fn test_restart_reuses_versions_and_collects_lost_forks() {
    let mut sim = Sim::new();
    let a = sim.extend(GENESIS, &frs(&[10, 20]));
    let b = sim.extend(a, &frs(&[15]));
    let c = sim.extend(b, &frs(&[17]));
    let lost = sim.extend(a, &frs(&[16]));
    let lost_child = sim.extend(lost, &frs(&[18]));
    sim.advance_lib(a);
    let probes = frs(&[15, 16, 17, 18, 19]);
    sim.check(&probes);
    sim.restart(&[lost].into());
    assert!(!sim.blocks.contains_key(&lost) && !sim.blocks.contains_key(&lost_child));
    sim.check(&probes);
    sim.advance_lib(c);
    sim.check(&probes);
    sim.assert_no_garbage();
}

#[test]
fn test_errors_leave_the_state_unchanged() {
    let mut sim = Sim::new();
    let a = sim.extend(GENESIS, &frs(&[10]));
    let imt = sim.blocks[&a].imt.insert(fr(20)).unwrap();
    // in the base, in the overlay, in the pending block
    sim.advance_lib(a);
    let base = &sim.blocks[&a].imt;
    assert_eq!(
        base.insert(fr(10)).unwrap_err(),
        ImtError::AlreadyPresent(fr(10))
    );
    assert_eq!(
        imt.insert(fr(20)).unwrap_err(),
        ImtError::AlreadyPresent(fr(20))
    );
    assert_eq!(imt.insert(Fr::ZERO).unwrap_err(), ImtError::ZeroNullifier);
    assert_eq!(imt.len(), 3);
    assert_eq!(
        imt.finalize(block_id(99)).unwrap_err(),
        ImtError::UncommittedBlock
    );
    assert_eq!(imt.snapshot().unwrap_err(), ImtError::UncommittedBlock);
    assert_eq!(
        Tree::open(Arc::new(MemKv::new())).unwrap_err(),
        ImtError::NotInitialized
    );
}

#[test]
fn test_full_tree() {
    type Small = Imt<Store, ZkHasher, 2>;
    let store = Arc::new(MemKv::new());
    let imt = Small::initialize(Arc::clone(&store), block_id(0)).unwrap();
    let imt = imt
        .insert(fr(1))
        .unwrap()
        .insert(fr(2))
        .unwrap()
        .insert(fr(3))
        .unwrap();
    assert_eq!(imt.insert(fr(4)).unwrap_err(), ImtError::Full);
    let mut oracle = IndexedMerkleTree::<ZkHasher, 2>::new();
    for n in 1..=3 {
        oracle = oracle.insert(fr(n)).unwrap();
    }
    assert_eq!(imt.root().unwrap(), oracle.root());
}

#[test]
fn test_order_is_integer_order_near_modulus() {
    let mut sim = Sim::new();
    let big = -Fr::ONE;
    let mid = Fr::from(num_bigint::BigUint::from(1u8) << 200);
    let a = sim.extend(GENESIS, &[big, mid]);
    sim.advance_lib(a);
    let b = sim.extend(a, &[fr(5), -Fr::from(2)]);
    sim.check(&[fr(4), fr(6), mid - Fr::ONE, mid + Fr::ONE, -Fr::from(3)]);
    sim.advance_lib(b);
    sim.check(&[fr(4), fr(6), mid - Fr::ONE, mid + Fr::ONE, -Fr::from(3)]);
}

#[test]
fn test_reseal_after_more_inserts() {
    let sim = Sim::new();
    let genesis = &sim.blocks[&GENESIS].imt;
    let sealed = genesis.insert(fr(7)).unwrap().seal().unwrap();
    let resealed = sealed.insert(fr(3)).unwrap().seal().unwrap();
    let oracle = Oracle::new().insert(fr(7)).unwrap().insert(fr(3)).unwrap();
    assert_eq!(resealed.root().unwrap(), oracle.root());
}

/// A store whose reads fail.
struct FailingKv;

#[derive(Debug, thiserror::Error)]
#[error("disk on fire")]
struct DiskError;

impl KvStore for FailingKv {
    type Error = DiskError;

    fn get(&self, _key: &[u8]) -> Result<Option<Bytes>, Self::Error> {
        Err(DiskError)
    }

    fn get_prev(&self, _prefix: &[u8], _key: &[u8]) -> Result<Option<KvEntry>, Self::Error> {
        Err(DiskError)
    }

    fn scan_prefix(&self, _prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error> {
        Err(DiskError)
    }

    fn write_batch(&self, _batch: WriteBatch) -> Result<(), Self::Error> {
        Err(DiskError)
    }
}

#[test]
fn test_storage_errors_propagate() {
    let store = Arc::new(FailingKv);
    assert!(matches!(
        Imt::<_, ZkHasher, 40>::open(Arc::clone(&store)),
        Err(ImtError::Storage(_))
    ));
    let imt = Imt::<_, ZkHasher, 40>::base_state(
        Arc::clone(&store),
        BaseMeta {
            len: 1,
            root: Imt::<Arc<FailingKv>, ZkHasher, 40>::empty_child(40),
            block_id: block_id(0),
        },
    );
    assert!(matches!(imt.insert(fr(1)), Err(ImtError::Storage(_))));
    assert!(matches!(imt.contains(&fr(1)), Err(ImtError::Storage(_))));
    assert!(matches!(
        Imt::<_, ZkHasher, 40>::collect_garbage(&store, 1),
        Err(ImtError::Storage(_))
    ));
}

#[derive(Clone, Debug)]
enum Op {
    Extend { parent: u64, nullifiers: Vec<u64> },
    AdvanceLib { pick: u64 },
    Pin { pick: u64 },
    Unpin { pick: u64 },
    Restart { lose: u64 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (any::<u64>(), prop::collection::vec(0u64..64, 0..5))
            .prop_map(|(parent, nullifiers)| Op::Extend { parent, nullifiers }),
        2 => any::<u64>().prop_map(|pick| Op::AdvanceLib { pick }),
        1 => any::<u64>().prop_map(|pick| Op::Pin { pick }),
        1 => any::<u64>().prop_map(|pick| Op::Unpin { pick }),
        1 => any::<u64>().prop_map(|lose| Op::Restart { lose }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    // Random forks over a small nullifier domain, so siblings often share low
    // leaves, with snapshots pinned below the LIB: every state matches its
    // oracle, no reachable node is missing and the garbage is only what the
    // pins retain.
    #[test]
    fn test_forks_match_oracle(ops in prop::collection::vec(op(), 1..24)) {
        let mut sim = Sim::new();
        let probes = frs(&[0, 1, 17, 31, 32, 63, 64, 1000]);
        for op in ops {
            match op {
                Op::Extend { parent, nullifiers } => {
                    let parent = sim.pick(parent);
                    sim.extend(parent, &frs(&nullifiers));
                }
                Op::AdvanceLib { pick } => {
                    let new_lib = sim.pick(pick);
                    sim.advance_lib(new_lib);
                }
                Op::Pin { pick } => {
                    let id = sim.pick(pick);
                    sim.pin(id);
                }
                Op::Unpin { pick } => {
                    let pinned: Vec<u64> = sim.pins.keys().copied().collect();
                    if let Some(id) = pinned.get(pick as usize % pinned.len().max(1)) {
                        sim.unpin(*id);
                    }
                }
                Op::Restart { lose } => {
                    let candidates: Vec<u64> = sim
                        .live()
                        .into_iter()
                        .filter(|id| *id != sim.lib)
                        .collect();
                    let lost: BTreeSet<u64> = candidates
                        .get(lose as usize % candidates.len().max(1))
                        .copied()
                        .filter(|_| lose % 2 == 0)
                        .into_iter()
                        .collect();
                    sim.restart(&lost);
                }
            }
            sim.check(&probes);
        }
        // with every pin released, nothing but the live states is stored
        let pinned: Vec<u64> = sim.pins.keys().copied().collect();
        for id in pinned {
            sim.unpin(id);
        }
        sim.collect_garbage();
        sim.check(&probes);
        sim.assert_no_garbage();
    }
}
