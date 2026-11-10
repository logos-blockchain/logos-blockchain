//! The storage of the nullifier trees of the ledger states.
//!
//! Every state of a [`Ledger`](crate::Ledger) keeps its nullifier trees in one
//! store (see [`NoteSets`]), copy on write: a committed block writes its nodes
//! under its own [`Version`], so the states of sibling forks never overwrite
//! each other. The store follows the chain:
//!
//! - **Commit** ([`NullifierStorage::commit`]): a block applied on top of its
//!   parent gets a version from the [`VersionRegistry`], and its pending tree
//!   blocks are written under it, in one batch with the registry entry and the
//!   list of the note sets the block wrote to.
//! - **Pruning** ([`NullifierStorage::prune`]), once the LIB advances, in one
//!   batch: the note sets of the new LIB become the finalized base of the
//!   store, everything the blocks of the pruned stale forks wrote is deleted
//!   from the sets they listed, and the versions of the pruned blocks and of
//!   the LIB are forgotten.
//! - **Garbage collection** ([`NullifierStorage::collect_garbage`]), right
//!   after: the nodes the finalized blocks replaced are deleted once no state
//!   still kept reads them. See [`retention_floors`].
//!
//! A state holds three note sets, each one a version of every nullifier tree:
//! its own, and the snapshots of the current and next epochs taken at the
//! stake distribution freeze. They are all canonical states once their block is
//! final, the snapshots reading the store below its finalized base.

use std::{collections::HashMap, sync::Arc};

use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::mantle::note_set::{CommittedSets, NoteSetError, NoteSetIndex, NoteSets};
use lb_imt::store::{BlockId, ImtError, LiveBlock, Version, VersionRegistry};
use lb_kv_store::{DynKvStore, KvStore as _, Prefixed, WriteBatch};

use crate::cryptarchia::LedgerState as CryptarchiaLedger;

/// The namespace of the [`VersionRegistry`] in the store, next to those of the
/// note sets (see [`lb_core::mantle::note_set::note_set_namespace`]).
pub const VERSIONS_NAMESPACE: &[u8] = b"V";

/// The namespace of the note sets each committed block wrote to, by version,
/// so that pruning the block deletes its writes in those sets alone.
pub const WRITTEN_NAMESPACE: &[u8] = b"W";

// The key of the sets the block of `version` wrote to.
fn written_key(version: Version) -> Vec<u8> {
    version.0.to_be_bytes().to_vec()
}

/// The store of the nullifier trees of a ledger, and the versions of its
/// committed blocks.
#[derive(Clone)]
pub struct NullifierStorage {
    store: DynKvStore,
    versions: VersionRegistry<Prefixed<DynKvStore>>,
    versions_store: Prefixed<DynKvStore>,
    written_store: Prefixed<DynKvStore>,
}

/// The blocks [`NullifierStorage::prune`] removes from the store.
pub struct PrunedVersions<'a> {
    /// The blocks of the forks that diverged below the new LIB: everything
    /// they wrote is deleted.
    pub stale: &'a [BlockId],
    /// The canonical blocks below the new LIB: their nodes stay, read by the
    /// LIB and its descendants, and only their versions are forgotten.
    pub immutable: &'a [BlockId],
}

impl NullifierStorage {
    #[must_use]
    pub fn new(store: DynKvStore) -> Self {
        let versions_store = Prefixed::new(Arc::clone(&store), VERSIONS_NAMESPACE);
        let written_store = Prefixed::new(Arc::clone(&store), WRITTEN_NAMESPACE);
        Self {
            store,
            versions: VersionRegistry::new(versions_store.clone()),
            versions_store,
            written_store,
        }
    }

    #[must_use]
    pub const fn store(&self) -> &DynKvStore {
        &self.store
    }

    fn write(&self, batch: WriteBatch) -> Result<(), NoteSetError> {
        if batch.is_empty() {
            return Ok(());
        }
        self.store
            .write_batch(batch)
            .map_err(|error| NoteSetError::Imt(ImtError::Storage(Arc::new(error))))
    }

    // The writes of the registry, as writes of the store.
    fn versions_batch(&self, writes: WriteBatch) -> WriteBatch {
        self.versions_store.namespace_batch(writes)
    }

    // The writes forgetting the version of `block_id`, and the sets it wrote
    // to, if it has one.
    fn release(&self, block_id: BlockId, batch: &mut WriteBatch) -> Result<(), NoteSetError> {
        if let Some(version) = self.versions.lookup(block_id)? {
            batch.append(self.versions_batch(
                VersionRegistry::<Prefixed<DynKvStore>>::release_writes(LiveBlock {
                    version,
                    block_id,
                }),
            ));
            let mut written = WriteBatch::new();
            written.delete(written_key(version));
            batch.append(self.written_store.namespace_batch(written));
        }
        Ok(())
    }

    // The writes recording that the block of `version` wrote to `sets`.
    fn record_written(
        &self,
        version: Version,
        sets: &[NoteSetIndex],
    ) -> Result<WriteBatch, NoteSetError> {
        let mut written = WriteBatch::new();
        if !sets.is_empty() {
            let bytes = sets.to_bytes().map_err(|error| {
                NoteSetError::Imt(ImtError::Corrupted(format!("written note sets: {error}")))
            })?;
            written.put(written_key(version), bytes);
        }
        Ok(self.written_store.namespace_batch(written))
    }

    /// The note sets the block of `version` wrote to, none if it wrote nothing.
    pub fn written(&self, version: Version) -> Result<Vec<NoteSetIndex>, NoteSetError> {
        let Some(bytes) = self
            .written_store
            .get(&written_key(version))
            .map_err(|error| NoteSetError::Imt(ImtError::Storage(Arc::new(error))))?
        else {
            return Ok(Vec::new());
        };
        Vec::<NoteSetIndex>::from_bytes(&bytes).map_err(|error| {
            NoteSetError::Imt(ImtError::Corrupted(format!("written note sets: {error}")))
        })
    }

    /// Commits the state of block `block_id`, written in the store before it
    /// is returned. A block replayed at restart gets the version it had, and
    /// writes the same nodes again.
    pub fn commit(
        &self,
        block_id: BlockId,
        state: CryptarchiaLedger,
    ) -> Result<CryptarchiaLedger, NoteSetError> {
        let allocation = self.versions.allocate(block_id)?;
        let (state, committed) = commit_state(state, allocation.version)?;
        let mut batch = self.versions_batch(allocation.writes);
        batch.append(committed.writes);
        batch.append(self.record_written(allocation.version, &committed.written)?);
        self.write(batch)?;
        Ok(state)
    }

    /// Makes the state of block `block_id`, the root of a ledger, the finalized
    /// base of the store: committed first if it has a pending block, as the
    /// genesis state does. A state restored from the store already is its base.
    pub fn make_root(
        &self,
        block_id: BlockId,
        state: CryptarchiaLedger,
    ) -> Result<CryptarchiaLedger, NoteSetError> {
        let state = if has_pending(&state) {
            self.commit(block_id, state)?
        } else {
            state
        };
        let mut batch = WriteBatch::new();
        let state = finalize_state(state, block_id, &mut batch)?;
        self.release(block_id, &mut batch)?;
        self.write(batch)?;
        self.collect_garbage(&state, std::iter::once(&state))?;
        Ok(state)
    }

    /// Makes `lib`, the state of block `lib_id`, the finalized base of the
    /// store, and removes the `pruned` blocks, in one batch. A stale block's
    /// writes are deleted from the note sets it recorded at its commit, so the
    /// states of the stale forks are not needed. Every state kept
    /// afterwards must be `lib`, one of its descendants, or one of their epoch
    /// snapshots: the others read a base that is not theirs.
    pub fn prune(
        &self,
        lib_id: BlockId,
        lib: CryptarchiaLedger,
        pruned: &PrunedVersions<'_>,
    ) -> Result<CryptarchiaLedger, NoteSetError> {
        let mut batch = WriteBatch::new();
        let lib = finalize_state(lib, lib_id, &mut batch)?;
        for block_id in pruned.stale {
            if let Some(version) = self.versions.lookup(*block_id)? {
                batch.append(NoteSets::discard_writes(
                    &self.store,
                    self.written(version)?,
                    version,
                ));
            }
            self.release(*block_id, &mut batch)?;
        }
        for block_id in pruned.immutable.iter().chain(std::iter::once(&lib_id)) {
            self.release(*block_id, &mut batch)?;
        }
        self.write(batch)?;
        Ok(lib)
    }

    /// Deletes the nodes the finalized blocks of `lib` replaced that no state
    /// of `kept` reads anymore, `kept` holding `lib` and every state the ledger
    /// keeps.
    pub fn collect_garbage<'a>(
        &self,
        lib: &CryptarchiaLedger,
        kept: impl IntoIterator<Item = &'a CryptarchiaLedger>,
    ) -> Result<(), NoteSetError> {
        let floors = retention_floors(kept);
        let mut batch = WriteBatch::new();
        // Only the sets of the LIB have a finalized base, and so replaced nodes
        // waiting for the collection.
        for (index, _) in lib.note_sets.nullifier_lens() {
            if let Some(floor) = floors.get(&index) {
                batch.append(NoteSets::collect_garbage(&self.store, index, *floor)?);
            }
        }
        self.write(batch)
    }
}

/// The retention floor of each note set: the length of the shortest of its
/// trees that a kept state reads, among the three note sets of each state.
///
/// The collection deletes the nodes a finalized block replaced once every
/// state is at least as long as the tree after that block: such a state reads
/// the nodes that replaced them instead. A tree only grows along a chain, and a
/// state is a descendant of the LIB or the LIB itself, so the floor is the
/// length of the oldest tree still read: usually the epoch snapshot of the LIB,
/// taken up to an epoch below it, which leader proofs read.
///
/// The floor is computed from every kept state, not from the LIB alone, so
/// that it does not depend on the snapshots of the descendants being at least
/// as long as those of the LIB. A set a state does not hold yet, a channel set
/// created after its snapshot was taken, does not bound the floor of that set.
pub fn retention_floors<'a>(
    kept: impl IntoIterator<Item = &'a CryptarchiaLedger>,
) -> HashMap<NoteSetIndex, u64> {
    let mut floors = HashMap::new();
    for state in kept {
        for note_sets in held_note_sets(state) {
            for (index, len) in note_sets.nullifier_lens() {
                floors
                    .entry(index)
                    .and_modify(|floor: &mut u64| *floor = (*floor).min(len))
                    .or_insert(len);
            }
        }
    }
    floors
}

/// Drops from the memory of `state` what the finalized base of the store holds.
pub fn rebase_state(state: CryptarchiaLedger) -> Result<CryptarchiaLedger, NoteSetError> {
    let mut state = state;
    state.note_sets = state.note_sets.rebase()?;
    state.epoch_state.note_sets = state.epoch_state.note_sets.rebase()?;
    state.next_epoch_state.note_sets = state.next_epoch_state.note_sets.rebase()?;
    Ok(state)
}

// The three note sets of a state: its own and its two epoch snapshots.
const fn held_note_sets(state: &CryptarchiaLedger) -> [&NoteSets; 3] {
    [
        &state.note_sets,
        &state.epoch_state.note_sets,
        &state.next_epoch_state.note_sets,
    ]
}

fn has_pending(state: &CryptarchiaLedger) -> bool {
    held_note_sets(state)
        .iter()
        .any(|note_sets| note_sets.has_pending())
}

// Commits the note sets of `state` under `version`. The epoch snapshots are
// those of the parent state, committed already, except in a genesis state,
// where they are the note sets of the state itself and committed with them.
// The returned sets are those of the state; their writes cover the snapshots.
fn commit_state(
    state: CryptarchiaLedger,
    version: Version,
) -> Result<(CryptarchiaLedger, CommittedSets), NoteSetError> {
    let mut state = state;
    let committed = state.note_sets.commit(version)?;
    let snapshot = |note_sets: NoteSets| {
        if !note_sets.has_pending() {
            Ok(note_sets)
        } else if note_sets == state.note_sets {
            Ok(committed.sets.clone())
        } else {
            Err(NoteSetError::Imt(ImtError::UncommittedBlock))
        }
    };
    state.epoch_state.note_sets = snapshot(state.epoch_state.note_sets.clone())?;
    state.next_epoch_state.note_sets = snapshot(state.next_epoch_state.note_sets.clone())?;
    state.note_sets = committed.sets.clone();
    Ok((state, committed))
}

fn finalize_state(
    state: CryptarchiaLedger,
    block_id: BlockId,
    batch: &mut WriteBatch,
) -> Result<CryptarchiaLedger, NoteSetError> {
    let mut state = state;
    let (finalized, writes) = state.note_sets.finalize(block_id)?;
    batch.append(writes);
    state.note_sets = finalized;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use lb_core::{
        crypto::ZkHasher,
        events::Events,
        mantle::{
            note_set::{NoteSet, note_set_namespace},
            notes::NoteNullifier,
            ops::channel::ChannelId,
        },
    };
    use lb_cryptarchia_engine::{Cryptarchia, Slot, State, UncleSlots};
    use lb_groth16::Fr;
    use lb_imt::{
        IndexedMerkleTree, LeafIndex,
        store::{NodeKey, StaleKey},
    };
    use lb_kv_store::{ErasedKv, MemKv};

    use super::*;
    use crate::{
        Ledger, LedgerState,
        cryptarchia::tests::{GENESIS_POW_REWARD_POOL, config},
        test_utils::single_era,
        update::BatchVerifiedUpdate,
    };

    type Id = [u8; 32];
    type Oracle = IndexedMerkleTree<ZkHasher, 40>;

    const GENESIS: Id = [0; 32];
    const LEDGER_SET: NoteSetIndex = NoteSetIndex::Ledger;
    const PROBES: [u64; 9] = [1, 3, 5, 8, 13, 21, 40, 77, 500];

    fn channel(n: u8) -> ChannelId {
        ChannelId::from([n; 32])
    }

    // The set of the channel created in the tests.
    fn channel_set() -> NoteSetIndex {
        NoteSetIndex::Channel(channel(2))
    }

    // A block creating the set of `channel`.
    fn create_channel(channel: ChannelId) -> impl FnOnce(LedgerState) -> LedgerState {
        move |mut state| {
            state.cryptarchia_ledger.note_sets = state
                .cryptarchia_ledger
                .note_sets
                .clone()
                .with_channel(channel);
            state
        }
    }

    fn fr(n: u64) -> Fr {
        Fr::from(n)
    }

    // A path as its sides and siblings, `MerkleNode` having no `PartialEq`.
    fn nodes<const N: usize>(path: &[lb_dynamic_merkle::MerkleNode<Fr>; N]) -> Vec<(bool, Fr)> {
        path.iter()
            .map(|node| {
                (
                    matches!(node, lb_dynamic_merkle::MerkleNode::Left(_)),
                    *node.item(),
                )
            })
            .collect()
    }

    // The nullifier tree of `set` reads as `oracle` does.
    fn check_set(name: &str, set: &NoteSet, oracle: &Oracle, probes: &[u64]) {
        let tree = set.nullifiers();
        assert_eq!(tree.len(), oracle.len() as u64, "len of {name}");
        assert_eq!(
            set.nullifier_root().unwrap(),
            oracle.root(),
            "root of {name}"
        );
        for probe in probes.iter().copied().map(fr) {
            assert_eq!(
                tree.contains(&probe).unwrap(),
                oracle.contains(&probe),
                "contains {probe} in {name}"
            );
            let low = tree.low_leaf(&probe).unwrap();
            assert_eq!(
                low.map(|low| (low.index.as_u64(), low.leaf)),
                oracle.low_leaf(&probe).map(|(index, leaf)| (index, *leaf)),
                "low leaf of {probe} in {name}"
            );
            if let Some((index, _)) = oracle.low_leaf(&probe) {
                assert_eq!(
                    tree.path(LeafIndex::new(index))
                        .unwrap()
                        .as_ref()
                        .map(nodes),
                    oracle.path(index).as_ref().map(nodes),
                    "path {index} in {name}"
                );
            }
        }
    }

    fn spend(note_sets: &NoteSets, index: NoteSetIndex, nullifiers: &[u64]) -> NoteSets {
        let spent: Vec<NoteNullifier> = nullifiers.iter().map(|n| NoteNullifier(fr(*n))).collect();
        let set = note_sets
            .get(index)
            .unwrap()
            .clone()
            .execute_spending(&spent)
            .unwrap();
        note_sets.clone().update(index, set)
    }

    fn insert_all(oracle: &Oracle, nullifiers: &[u64]) -> Oracle {
        nullifiers
            .iter()
            .fold(oracle.clone(), |oracle, n| oracle.insert(fr(*n)).unwrap())
    }

    /// A ledger and the consensus engine driving its pruning, with what each
    /// set of each kept block should hold.
    struct Chain {
        ledger: Ledger<Id>,
        engine: Cryptarchia<Id>,
        store: Arc<MemKv>,
        oracles: HashMap<Id, HashMap<NoteSetIndex, Oracle>>,
        next: u8,
    }

    impl Chain {
        fn new() -> Self {
            let config = config();
            let store = Arc::new(MemKv::new());
            let note_sets = NoteSets::with_store(ErasedKv::into_dyn(Arc::clone(&store)));
            let mut state = LedgerState::from_notes([], &config, GENESIS_POW_REWARD_POOL);
            state.cryptarchia_ledger.note_sets = note_sets.clone();
            state.cryptarchia_ledger.epoch_state.note_sets = note_sets.clone();
            state.cryptarchia_ledger.next_epoch_state.note_sets = note_sets;
            let eras = single_era(config);
            let engine = Cryptarchia::from_genesis(
                GENESIS,
                Arc::new(eras.map(|era| era.parameters.consensus_config.clone())),
                State::Online,
            );
            let ledger = Ledger::new(GENESIS, state, Arc::new(eras)).unwrap();
            let genesis = HashMap::from([(LEDGER_SET, Oracle::new())]);
            Self {
                ledger,
                engine,
                store,
                oracles: HashMap::from([(GENESIS, genesis)]),
                next: 1,
            }
        }

        fn state(&self, id: &Id) -> &CryptarchiaLedger {
            &self.ledger.state(id).unwrap().cryptarchia_ledger
        }

        fn extend(&mut self, parent: Id, nullifiers: &[u64]) -> Id {
            self.extend_with(parent, &[(LEDGER_SET, nullifiers)], |state| state)
        }

        // Applies a block on `parent`: `amend` to its state, then the spending of
        // `spent` in each set. Then prunes what the consensus prunes.
        fn extend_with(
            &mut self,
            parent: Id,
            spent: &[(NoteSetIndex, &[u64])],
            amend: impl FnOnce(LedgerState) -> LedgerState,
        ) -> Id {
            let id = [self.next; 32];
            let slot = Slot::from(u64::from(self.next));
            self.next += 1;
            let mut state = self.ledger.state(&parent).unwrap().clone();
            state.block_number += 1;
            let mut state = amend(state);
            let mut note_sets = state.cryptarchia_ledger.note_sets.clone();
            let mut oracles = self.oracles[&parent].clone();
            for (index, nullifiers) in spent {
                note_sets = spend(&note_sets, *index, nullifiers);
                let oracle = oracles.entry(*index).or_default();
                *oracle = insert_all(oracle, nullifiers);
            }
            state.cryptarchia_ledger.note_sets = note_sets.end_block(state.block_number).unwrap();
            self.ledger
                .commit_update(BatchVerifiedUpdate {
                    id,
                    state,
                    events: Events::new(),
                })
                .unwrap();
            self.oracles.insert(id, oracles);
            let (pruned, _) = self
                .engine
                .receive_block(id, parent, slot, UncleSlots::default())
                .unwrap();
            self.ledger
                .apply_pruning(self.engine.lib(), &pruned)
                .unwrap();
            let ledger = &self.ledger;
            self.oracles
                .retain(|block, _| ledger.state(block).is_some());
            id
        }

        // Every kept state reads as its oracles.
        fn check(&self, probes: &[u64]) {
            for (id, oracles) in &self.oracles {
                let note_sets = &self.state(id).note_sets;
                for (index, oracle) in oracles {
                    check_set(
                        &format!("set {index} of block {}", id[0]),
                        note_sets.get(*index).unwrap(),
                        oracle,
                        probes,
                    );
                }
            }
        }

        fn version(&self, id: Id) -> Version {
            let versions = VersionRegistry::new(Prefixed::new(
                ErasedKv::into_dyn(Arc::clone(&self.store)),
                VERSIONS_NAMESPACE,
            ));
            versions.lookup(BlockId(id)).unwrap().unwrap()
        }

        // The keys the block of `version` wrote in the set `index`.
        fn written(&self, index: NoteSetIndex, version: Version) -> usize {
            let set_store = Prefixed::new(Arc::clone(&self.store), note_set_namespace(index));
            let nodes = set_store
                .scan_prefix(&NodeKey::version_prefix(version))
                .unwrap();
            let stale = set_store
                .scan_prefix(&StaleKey::version_prefix(version))
                .unwrap();
            nodes.len() + stale.len()
        }
    }

    #[test]
    fn test_chain_reads_as_the_oracle_while_the_lib_advances() {
        let mut chain = Chain::new();
        let mut tip = GENESIS;
        for block in 0..8u64 {
            let nullifiers: Vec<u64> = (0..3).map(|i| (block * 31 + i * 7) % 90 + 1).collect();
            tip = chain.extend(tip, &nullifiers);
            chain.check(&PROBES);
        }
        assert_ne!(chain.engine.lib(), GENESIS, "the LIB advanced");
        assert!(
            chain.ledger.state(&GENESIS).is_none(),
            "below the LIB is pruned"
        );
        let lib = chain.state(&chain.engine.lib());
        assert!(
            lib.note_sets
                .get(LEDGER_SET)
                .unwrap()
                .nullifiers()
                .is_finalized()
        );
    }

    #[test]
    fn test_a_pruned_fork_leaves_nothing_in_the_store() {
        let mut chain = Chain::new();
        let b1 = chain.extend(GENESIS, &[10, 30]);
        // both rewrite the low leaf of 10
        let fork = chain.extend(b1, &[20]);
        let doomed = chain.state(&fork).note_sets.clone();
        let fork_version = chain.version(fork);
        assert!(chain.written(LEDGER_SET, fork_version) > 0);
        let mut tip = chain.extend(b1, &[25]);
        chain.check(&PROBES);
        while chain.ledger.state(&fork).is_some() {
            tip = chain.extend(tip, &[]);
        }
        assert_eq!(chain.written(LEDGER_SET, fork_version), 0);
        assert!(matches!(
            doomed
                .get(LEDGER_SET)
                .unwrap()
                .nullifiers()
                .path(LeafIndex::new(3)),
            Err(ImtError::MissingNode(_))
        ));
        chain.check(&PROBES);
    }

    #[test]
    fn test_an_epoch_snapshot_below_the_lib_stays_readable_until_replaced() {
        let mut chain = Chain::new();
        let b1 = chain.extend(GENESIS, &[10, 30, 50]);
        let snapshot = chain.state(&b1).note_sets.clone();
        let snapshot_oracle = chain.oracles[&b1][&LEDGER_SET].clone();
        // the snapshots are taken by the next block, as at the stake freeze
        let mut tip = chain.extend_with(b1, &[(LEDGER_SET, &[20])], |mut state| {
            state.cryptarchia_ledger.epoch_state.note_sets = snapshot.clone();
            state.cryptarchia_ledger.next_epoch_state.note_sets = snapshot.clone();
            state
        });
        for n in [40, 5, 60, 35] {
            tip = chain.extend(tip, &[n]);
        }
        assert!(
            chain.ledger.state(&b1).is_none(),
            "the snapshot is below the LIB"
        );
        let held = &chain.state(&tip).epoch_state.note_sets;
        check_set(
            "epoch snapshot",
            held.get(LEDGER_SET).unwrap(),
            &snapshot_oracle,
            &PROBES,
        );
        assert_eq!(
            retention_floors([chain.state(&tip)])[&LEDGER_SET],
            snapshot_oracle.len() as u64
        );

        // The next snapshots replace it: once no kept state holds the old one,
        // the nodes only it read are collected.
        let current = chain.state(&tip).note_sets.clone();
        tip = chain.extend_with(tip, &[(LEDGER_SET, &[70])], |mut state| {
            state.cryptarchia_ledger.epoch_state.note_sets = current.clone();
            state.cryptarchia_ledger.next_epoch_state.note_sets = current.clone();
            state
        });
        for n in [80, 90] {
            tip = chain.extend(tip, &[n]);
        }
        chain.check(&PROBES);
        assert!(matches!(
            snapshot
                .get(LEDGER_SET)
                .unwrap()
                .nullifiers()
                .path(LeafIndex::new(1)),
            Err(ImtError::MissingNode(_))
        ));
    }

    #[test]
    fn test_two_forks_create_the_same_channel_set() {
        let mut chain = Chain::new();
        let b1 = chain.extend(GENESIS, &[1]);
        let left = chain.extend_with(b1, &[(channel_set(), &[4, 9])], create_channel(channel(2)));
        let right = chain.extend_with(b1, &[(channel_set(), &[6])], create_channel(channel(2)));
        let right_version = chain.version(right);
        chain.check(&PROBES);
        let mut tip = left;
        while chain.ledger.state(&right).is_some() {
            let nullifier = u64::from(tip[0]) + 10;
            tip = chain.extend_with(tip, &[(channel_set(), &[nullifier])], |state| state);
        }
        assert_eq!(chain.written(channel_set(), right_version), 0);
        chain.check(&[1, 4, 6, 9, 12, 15]);
        assert!(
            chain
                .state(&chain.engine.lib())
                .note_sets
                .get(channel_set())
                .unwrap()
                .nullifiers()
                .is_finalized()
        );
    }

    #[test]
    fn test_a_pruned_fork_leaves_nothing_in_the_channel_it_created() {
        let mut chain = Chain::new();
        let b1 = chain.extend(GENESIS, &[1]);
        let doomed_set = NoteSetIndex::Channel(channel(3));
        let fork = chain.extend_with(b1, &[(doomed_set, &[6, 8])], create_channel(channel(3)));
        let fork_version = chain.version(fork);
        assert!(chain.written(doomed_set, fork_version) > 0);
        assert_eq!(
            chain.ledger.storage.written(fork_version).unwrap(),
            vec![doomed_set]
        );
        let mut tip = chain.extend(b1, &[2]);
        while chain.ledger.state(&fork).is_some() {
            tip = chain.extend(tip, &[]);
        }
        // The channel exists on no kept state, and the fork's writes in it are
        // gone with the record of them.
        assert!(chain.state(&tip).note_sets.channel(&channel(3)).is_none());
        assert_eq!(chain.written(doomed_set, fork_version), 0);
        assert_eq!(chain.ledger.storage.written(fork_version).unwrap(), []);
        chain.check(&PROBES);
    }

    #[test]
    fn test_a_restored_lib_state_keeps_reading() {
        let mut chain = Chain::new();
        let mut tip = GENESIS;
        for nullifiers in [[3, 9], [1, 7], [5, 2], [8, 4]] {
            tip = chain.extend(tip, &nullifiers);
        }
        let lib = chain.engine.lib();
        let saved = chain.ledger.state(&lib).unwrap().to_bytes().unwrap();
        let restored = LedgerState::from_bytes(&saved).unwrap();
        let ledger = Ledger::new(lib, restored, Arc::clone(chain.ledger.era_schedule())).unwrap();
        let state = &ledger.state(&lib).unwrap().cryptarchia_ledger;
        let oracle = &chain.oracles[&lib][&LEDGER_SET];
        check_set(
            "restored",
            state.note_sets.get(LEDGER_SET).unwrap(),
            oracle,
            &PROBES,
        );
        assert_eq!(*state, *chain.state(&lib));
    }

    #[test]
    fn test_retention_floors_take_the_shortest_held_tree_of_each_set() {
        let mut chain = Chain::new();
        let b1 = chain.extend(GENESIS, &[10]);
        let b1_sets = chain.state(&b1).note_sets.clone();
        let b2 = chain.extend_with(b1, &[(LEDGER_SET, &[20, 30])], |mut state| {
            state.cryptarchia_ledger.next_epoch_state.note_sets = b1_sets.clone();
            create_channel(channel(2))(state)
        });
        let floors = retention_floors([chain.state(&b2)]);
        // the sentinel only, in the genesis tree its epoch snapshot still holds
        assert_eq!(floors[&LEDGER_SET], 1);
        // a channel set no snapshot holds yet: its only tree, the sentinel
        assert_eq!(floors[&channel_set()], 1);

        let b3 = chain.extend_with(b2, &[], |mut state| {
            state.cryptarchia_ledger.epoch_state.note_sets = b1_sets.clone();
            state
        });
        // the sentinel and 10, in the snapshots of block 1
        assert_eq!(retention_floors([chain.state(&b3)])[&LEDGER_SET], 2);
        // the floor of a set is the shortest tree among every kept state
        assert_eq!(
            retention_floors([chain.state(&b3), chain.state(&b2)])[&LEDGER_SET],
            1
        );
    }
}
