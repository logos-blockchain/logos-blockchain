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
//!
//! Every finalization may also write an anchor under [`ANCHOR_KEY`], in its
//! batch: what a restart needs to read the store again, starting from its base
//! (see [`crate::Anchor`]). The store and the anchor never disagree, whatever
//! the moment the node stops.

use std::{collections::HashMap, sync::Arc};

use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::mantle::note_set::{CommittedSets, NoteSetError, NoteSetIndex, NoteSets};
use lb_imt::store::{BlockId, ImtError, LiveBlock, Version, VersionRegistry};
use lb_kv_store::{Bytes, DynKvStore, KvStore as _, Prefixed, WriteBatch};

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

/// The key of the anchor of the finalized base, next to the namespaces of the
/// note sets and of the [`VersionRegistry`].
pub const ANCHOR_KEY: &[u8] = b"A";

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

    /// Writes `batch` in the store, atomically.
    pub fn write(&self, batch: WriteBatch) -> Result<(), NoteSetError> {
        write(&self.store, batch)
    }

    /// The anchor written with the last finalization, if any.
    pub fn read_anchor(store: &DynKvStore) -> Result<Option<Bytes>, NoteSetError> {
        store.get(ANCHOR_KEY).map_err(storage_error)
    }

    /// Deletes everything `store` holds, as when what it holds has no anchor
    /// to be read again from.
    pub fn clear(store: &DynKvStore) -> Result<(), NoteSetError> {
        let mut batch = WriteBatch::new();
        batch.delete_prefix(Bytes::new());
        write(store, batch)
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
            batch.append(self.forget_written(version));
        }
        Ok(())
    }

    // The writes forgetting the sets the block of `version` wrote to.
    fn forget_written(&self, version: Version) -> WriteBatch {
        let mut written = WriteBatch::new();
        written.delete(written_key(version));
        self.written_store.namespace_batch(written)
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
            .map_err(storage_error)?
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
    ///
    /// Returns the batch finalizing it, for the caller to write, then collect
    /// the garbage.
    pub fn make_root(
        &self,
        block_id: BlockId,
        state: CryptarchiaLedger,
    ) -> Result<(CryptarchiaLedger, WriteBatch), NoteSetError> {
        let state = if has_pending(&state) {
            self.commit(block_id, state)?
        } else {
            state
        };
        let mut batch = WriteBatch::new();
        let state = finalize_state(state, block_id, &mut batch)?;
        self.release(block_id, &mut batch)?;
        Ok((state, batch))
    }

    /// Makes `lib`, the state of block `lib_id`, the finalized base of the
    /// store, and removes the `pruned` blocks, in one batch. A stale block's
    /// writes are deleted from the note sets it recorded at its commit, so the
    /// states of the stale forks are not needed. Every state kept
    /// afterwards must be `lib`, one of its descendants, or one of their epoch
    /// snapshots: the others read a base that is not theirs.
    ///
    /// Returns the batch doing it all, for the caller to write.
    pub fn prune(
        &self,
        lib_id: BlockId,
        lib: CryptarchiaLedger,
        pruned: &PrunedVersions<'_>,
    ) -> Result<(CryptarchiaLedger, WriteBatch), NoteSetError> {
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
        Ok((lib, batch))
    }

    /// Deletes what the blocks of the versions still recorded wrote, unless
    /// `kept` keeps them, and forgets their versions. After a restart, these
    /// are the blocks the replay did not apply again: forks lost with the
    /// stop, or blocks pruned by a pruning the stop interrupted. Their writes
    /// are deleted from the note sets each recorded at its commit.
    ///
    /// Returns the number of blocks discarded.
    pub fn discard_lost(&self, kept: impl Fn(BlockId) -> bool) -> Result<usize, NoteSetError> {
        let mut batch = WriteBatch::new();
        let mut discarded = 0usize;
        for live in self.versions.live_blocks()? {
            if kept(live.block_id) {
                continue;
            }
            batch.append(NoteSets::discard_writes(
                &self.store,
                self.written(live.version)?,
                live.version,
            ));
            batch.append(self.forget_written(live.version));
            batch.append(self.versions_batch(
                VersionRegistry::<Prefixed<DynKvStore>>::release_writes(live),
            ));
            discarded = discarded.saturating_add(1);
        }
        self.write(batch)?;
        Ok(discarded)
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

/// The note sets of `state`, decoded from an anchor, attached to `store`: those
/// of the state itself must be its finalized base.
pub fn attach_state(
    state: CryptarchiaLedger,
    store: &DynKvStore,
) -> Result<CryptarchiaLedger, NoteSetError> {
    let mut state = state;
    state.note_sets = state.note_sets.attach(Arc::clone(store))?;
    state.note_sets.check_base()?;
    state.epoch_state.note_sets = state.epoch_state.note_sets.attach(Arc::clone(store))?;
    state.next_epoch_state.note_sets =
        state.next_epoch_state.note_sets.attach(Arc::clone(store))?;
    Ok(state)
}

/// The note sets of `state`, never committed, moved to `store` (see
/// [`NoteSets::move_to`]).
pub fn move_state(
    state: CryptarchiaLedger,
    store: &DynKvStore,
) -> Result<CryptarchiaLedger, NoteSetError> {
    let mut state = state;
    state.note_sets = state.note_sets.move_to(Arc::clone(store))?;
    state.epoch_state.note_sets = state.epoch_state.note_sets.move_to(Arc::clone(store))?;
    state.next_epoch_state.note_sets = state
        .next_epoch_state
        .note_sets
        .move_to(Arc::clone(store))?;
    Ok(state)
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

fn storage_error(error: lb_kv_store::StoreError) -> NoteSetError {
    NoteSetError::Imt(ImtError::Storage(Arc::new(error)))
}

fn write(store: &DynKvStore, batch: WriteBatch) -> Result<(), NoteSetError> {
    if batch.is_empty() {
        return Ok(());
    }
    store.write_batch(batch).map_err(storage_error)
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
        Ledger, LedgerState, Restored,
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

    // A block as applied, to be applied again after a restart.
    #[derive(Clone)]
    struct Applied {
        parent: Id,
        spent: Vec<(NoteSetIndex, Vec<u64>)>,
    }

    /// A ledger and the consensus engine driving its pruning, with what each
    /// set of each kept block should hold. Each finalization writes an anchor
    /// whose info is the first byte of the id of the LIB.
    struct Chain {
        ledger: Ledger<Id>,
        engine: Cryptarchia<Id>,
        store: DynKvStore,
        oracles: HashMap<Id, HashMap<NoteSetIndex, Oracle>>,
        applied: HashMap<Id, Applied>,
        next: u8,
    }

    // A block on `parent` spending `spent`, applied on the state of `parent` in
    // `ledger` after `amend`.
    fn build(
        ledger: &Ledger<Id>,
        id: Id,
        parent: Id,
        spent: &[(NoteSetIndex, Vec<u64>)],
        amend: impl FnOnce(LedgerState) -> LedgerState,
    ) -> BatchVerifiedUpdate<Id> {
        let mut state = ledger.state(&parent).unwrap().clone();
        state.block_number += 1;
        let mut state = amend(state);
        let mut note_sets = state.cryptarchia_ledger.note_sets.clone();
        for (index, nullifiers) in spent {
            note_sets = spend(&note_sets, *index, nullifiers);
        }
        state.cryptarchia_ledger.note_sets = note_sets.end_block(state.block_number).unwrap();
        BatchVerifiedUpdate {
            id,
            state,
            events: Events::new(),
        }
    }

    impl Chain {
        fn new() -> Self {
            Self::with_store(ErasedKv::into_dyn(MemKv::new()))
        }

        fn with_store(store: DynKvStore) -> Self {
            let config = config();
            // a genesis state is built in memory, then moved to the store
            let state = LedgerState::from_notes([], &config, GENESIS_POW_REWARD_POOL)
                .with_nullifier_store(&store)
                .unwrap();
            let eras = single_era(config);
            let engine = Cryptarchia::from_genesis(
                GENESIS,
                Arc::new(eras.map(|era| era.parameters.consensus_config.clone())),
                State::Online,
            );
            let ledger = Ledger::new_anchored(GENESIS, state, Arc::new(eras), &GENESIS[0]).unwrap();
            let genesis = HashMap::from([(LEDGER_SET, Oracle::new())]);
            Self {
                ledger,
                engine,
                store,
                oracles: HashMap::from([(GENESIS, genesis)]),
                applied: HashMap::new(),
                next: 1,
            }
        }

        fn dyn_store(&self) -> DynKvStore {
            Arc::clone(&self.store)
        }

        // The ledger a restart restores from the anchor of the store.
        fn restore(&self) -> Restored<Id, u8> {
            Ledger::restore(&self.dyn_store(), Arc::clone(self.ledger.era_schedule()))
                .unwrap()
                .expect("the store has an anchor")
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
            let id = self.commit(parent, spent, amend);
            let slot = Slot::from(u64::from(id[0]));
            let (pruned, _) = self
                .engine
                .receive_block(id, parent, slot, UncleSlots::default())
                .unwrap();
            let lib = self.engine.lib();
            self.ledger
                .apply_pruning_anchored(lib, &pruned, &lib[0])
                .unwrap();
            let ledger = &self.ledger;
            self.oracles
                .retain(|block, _| ledger.state(block).is_some());
            id
        }

        // Commits a block on `parent`, without telling the consensus: as when
        // the node stops before the pruning that follows.
        fn commit(
            &mut self,
            parent: Id,
            spent: &[(NoteSetIndex, &[u64])],
            amend: impl FnOnce(LedgerState) -> LedgerState,
        ) -> Id {
            let id = [self.next; 32];
            self.next += 1;
            let spent: Vec<(NoteSetIndex, Vec<u64>)> = spent
                .iter()
                .map(|(index, nullifiers)| (*index, nullifiers.to_vec()))
                .collect();
            let update = build(&self.ledger, id, parent, &spent, amend);
            self.ledger.commit_update(update).unwrap();
            let mut oracles = self.oracles[&parent].clone();
            for (index, nullifiers) in &spent {
                let oracle = oracles.entry(*index).or_default();
                *oracle = insert_all(oracle, nullifiers);
            }
            self.oracles.insert(id, oracles);
            self.applied.insert(id, Applied { parent, spent });
            id
        }

        // The blocks from the child of `lib` to `tip`, in order.
        fn path(&self, lib: Id, tip: Id) -> Vec<Id> {
            let mut path = Vec::new();
            let mut id = tip;
            while id != lib {
                path.push(id);
                id = self.applied[&id].parent;
            }
            path.reverse();
            path
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
            let versions =
                VersionRegistry::new(Prefixed::new(Arc::clone(&self.store), VERSIONS_NAMESPACE));
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
        assert_ne!(lib, GENESIS);
        let Restored {
            ledger,
            lib: restored_lib,
            info,
        } = chain.restore();
        assert_eq!((restored_lib, info), (lib, lib[0]));
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
    fn test_a_decoded_state_reads_nothing_until_attached() {
        let mut chain = Chain::new();
        let mut tip = GENESIS;
        for nullifiers in [[3, 9], [1, 7], [5, 2]] {
            tip = chain.extend(tip, &nullifiers);
        }
        let lib = chain.engine.lib();
        let saved = chain.ledger.state(&lib).unwrap().to_bytes().unwrap();
        let decoded = LedgerState::from_bytes(&saved).unwrap();
        let set = decoded.latest_note_sets().get(LEDGER_SET).unwrap();
        assert!(matches!(
            set.contains_nullifier(&NoteNullifier(fr(3))),
            Err(NoteSetError::Imt(ImtError::Storage(_)))
        ));
        let attached = attach_state(decoded.cryptarchia_ledger, &chain.dyn_store()).unwrap();
        assert_eq!(attached, *chain.state(&lib));
        // a tree above the base is not a canonical state to attach
        let saved = chain.ledger.state(&tip).unwrap().to_bytes().unwrap();
        let above = LedgerState::from_bytes(&saved).unwrap();
        assert!(attach_state(above.cryptarchia_ledger, &chain.dyn_store()).is_err());
    }

    #[test]
    fn test_restart_replays_the_chain_and_discards_lost_forks() {
        let mut chain = Chain::new();
        let mut tip = GENESIS;
        for nullifiers in [[3, 9], [1, 7], [5, 2], [8, 4]] {
            tip = chain.extend(tip, &nullifiers);
        }
        let lib = chain.engine.lib();
        // Committed before the stop, never seen by the consensus nor pruned:
        // two blocks on the chain, and a fork above the LIB the restart
        // forgets, as one received just before the stop.
        let lost = chain.commit(lib, &[(LEDGER_SET, &[60])], |state| state);
        let lost_version = chain.version(lost);
        assert!(chain.written(LEDGER_SET, lost_version) > 0);
        tip = chain.commit(tip, &[(LEDGER_SET, &[11])], |state| state);
        tip = chain.commit(tip, &[(LEDGER_SET, &[12, 13])], |state| state);

        let Restored { mut ledger, .. } = chain.restore();
        for id in chain.path(lib, tip) {
            let version = chain.version(id);
            let Applied { parent, spent } = chain.applied[&id].clone();
            let update = build(&ledger, id, parent, &spent, |state| state);
            ledger.commit_update(update).unwrap();
            assert_eq!(chain.version(id), version, "the version is reused");
            assert_eq!(
                ledger.state(&id).unwrap().cryptarchia_ledger,
                *chain.state(&id)
            );
        }
        assert_eq!(ledger.discard_lost_blocks().unwrap(), 1);
        assert_eq!(chain.written(LEDGER_SET, lost_version), 0);
        assert_eq!(ledger.discard_lost_blocks().unwrap(), 0);
        for id in chain.path(lib, tip) {
            check_set(
                &format!("replayed block {}", id[0]),
                ledger
                    .state(&id)
                    .unwrap()
                    .cryptarchia_ledger
                    .note_sets
                    .get(LEDGER_SET)
                    .unwrap(),
                &chain.oracles[&id][&LEDGER_SET],
                &PROBES,
            );
        }
    }

    #[test]
    fn test_restart_reopens_the_store_on_disk() {
        let directory = tempfile::tempdir().expect("a temporary directory is created");
        let open = || {
            ErasedKv::into_dyn(
                lb_kv_store::RocksKv::open(directory.path()).expect("the database opens"),
            )
        };
        let mut chain = Chain::with_store(open());
        let mut tip = GENESIS;
        for nullifiers in [[3, 9], [1, 7], [5, 2], [8, 4]] {
            tip = chain.extend(tip, &nullifiers);
        }
        let lost = chain.commit(chain.engine.lib(), &[(LEDGER_SET, &[60])], |state| state);
        tip = chain.commit(tip, &[(LEDGER_SET, &[11])], |state| state);
        let lib = chain.engine.lib();
        let path = chain.path(lib, tip);
        let (applied, oracles) = (chain.applied.clone(), chain.oracles.clone());
        // a state holds its store: its encoding is kept instead
        let lib_state = chain.state(&lib).to_bytes().unwrap();
        let eras = Arc::clone(chain.ledger.era_schedule());
        // the stop: every handle of the database is dropped, closing it
        drop(chain);

        let store = open();
        let Restored {
            mut ledger,
            lib: restored_lib,
            info,
        } = Ledger::<Id>::restore::<u8>(&store, eras)
            .unwrap()
            .expect("the store has an anchor");
        assert_eq!((restored_lib, info), (lib, lib[0]));
        assert_eq!(
            ledger
                .state(&lib)
                .unwrap()
                .cryptarchia_ledger
                .to_bytes()
                .unwrap(),
            lib_state
        );
        for id in &path {
            let Applied { parent, spent } = applied[id].clone();
            ledger
                .commit_update(build(&ledger, *id, parent, &spent, |state| state))
                .unwrap();
        }
        assert_eq!(ledger.discard_lost_blocks().unwrap(), 1);
        assert!(ledger.state(&lost).is_none());
        for id in path.iter().chain([&lib]) {
            check_set(
                &format!("block {} after the restart", id[0]),
                ledger
                    .state(id)
                    .unwrap()
                    .cryptarchia_ledger
                    .note_sets
                    .get(LEDGER_SET)
                    .unwrap(),
                &oracles[id][&LEDGER_SET],
                &PROBES,
            );
        }
    }

    #[test]
    fn test_a_store_without_anchor_restores_nothing() {
        let store = ErasedKv::into_dyn(MemKv::new());
        let config = config();
        let state = LedgerState::from_notes([], &config, GENESIS_POW_REWARD_POOL)
            .with_nullifier_store(&store)
            .unwrap();
        let eras = Arc::new(single_era(config));
        assert!(
            Ledger::<Id>::restore::<u8>(&store, Arc::clone(&eras))
                .unwrap()
                .is_none()
        );
        // a ledger created without anchor writes none
        let ledger = Ledger::new(GENESIS, state, Arc::clone(&eras)).unwrap();
        assert!(
            Ledger::<Id>::restore::<u8>(ledger.storage.store(), eras)
                .unwrap()
                .is_none()
        );
        assert_ne!(store.scan_prefix(b"").unwrap(), vec![]);
        NullifierStorage::clear(&store).unwrap();
        assert_eq!(store.scan_prefix(b"").unwrap(), vec![]);
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
