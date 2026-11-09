//! The indexed Merkle tree kept copy on write in a [`KvStore`].
//!
//! The nodes are stored immutable under the [`Version`] of the block that
//! wrote them, so the states of sibling forks never write the same key. A
//! state holds its root, its length and, in memory, the nullifiers inserted
//! since the finalized base; everything else is read from the store.
//!
//! A block goes through [`Imt::insert`], [`Imt::seal`] (hashing) and
//! [`Imt::commit`] (the writes of its nodes). Once it is final,
//! [`Imt::finalize`] moves its nullifiers to the base index and queues the
//! nodes it replaced, which [`Imt::collect_garbage`] deletes once no retained
//! state is older than the block; the nodes of a pruned fork go with
//! [`Imt::discard_writes`]. Every write is returned as a [`WriteBatch`] for the
//! caller to apply atomically with its own writes, before it uses the returned
//! state.
//!
//! A canonical state at or below the finalized base, such as an epoch
//! snapshot, is read from its length and root alone ([`Imt::from_snapshot`]):
//! the base index entries past its length are not part of it.

mod layout;
#[cfg(test)]
mod tests;
mod versions;

use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fmt,
    marker::PhantomData,
    sync::Arc,
};

use ark_ff::AdditiveGroup as _;
pub use layout::{
    BaseMeta, BlockId, BlockVersionKey, Child, CodecError, GcKey, GcMarker, IndexEntry,
    LiveVersion, LiveVersionKey, MetaKey, NextVersionKey, NodeKey, NodePosition, NullifierKey,
    StaleKey, StaleMarker, StoreKey, StoreValue, StoredNode, Version,
};
use lb_dynamic_merkle::{MerkleHasher as _, MerkleNode, MerklePath};
use lb_kv_store::{KvStore, WriteBatch};
use lb_poseidon2::{Digest, Fr};
use lb_utxotree::UtxoMerkleHasher;
use rpds::{HashTrieMapSync, QueueSync, RedBlackTreeMapSync, VectorSync};
pub use versions::{LiveBlock, VersionAllocation, VersionRegistry};

use crate::{LeafIndex, NullifierLeaf};

#[derive(Clone, Debug, thiserror::Error)]
pub enum ImtError {
    #[error("nullifier {0} is already in the tree")]
    AlreadyPresent(Fr),
    #[error("the zero nullifier is reserved for the sentinel leaf")]
    ZeroNullifier,
    #[error("the indexed Merkle tree is full")]
    Full,
    #[error("storage error: {0}")]
    Storage(#[source] Arc<dyn Error + Send + Sync>),
    #[error("node {0:?} is missing from the store")]
    MissingNode(NodeKey),
    #[error("corrupted store: {0}")]
    Corrupted(String),
    #[error("the store holds no tree")]
    NotInitialized,
    #[error("the store already holds a tree")]
    AlreadyInitialized,
    #[error("the state has a block that is not committed")]
    UncommittedBlock,
    #[error("the state does not match the finalized base of the store")]
    BaseMismatch,
}

impl PartialEq for ImtError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AlreadyPresent(a), Self::AlreadyPresent(b)) => a == b,
            (Self::MissingNode(a), Self::MissingNode(b)) => a == b,
            (Self::Corrupted(a), Self::Corrupted(b)) => a == b,
            (Self::Storage(a), Self::Storage(b)) => a.to_string() == b.to_string(),
            (Self::ZeroNullifier, Self::ZeroNullifier)
            | (Self::Full, Self::Full)
            | (Self::NotInitialized, Self::NotInitialized)
            | (Self::AlreadyInitialized, Self::AlreadyInitialized)
            | (Self::UncommittedBlock, Self::UncommittedBlock)
            | (Self::BaseMismatch, Self::BaseMismatch) => true,
            _ => false,
        }
    }
}

impl Eq for ImtError {}

impl ImtError {
    fn storage(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Storage(Arc::new(error))
    }

    fn corrupted(what: &str, error: &CodecError) -> Self {
        Self::Corrupted(format!("{what}: {error}"))
    }
}

/// A committed state as its length and root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub len: u64,
    pub root: Child,
}

/// The leaf of the greatest nullifier lower than a nullifier not in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LowLeaf {
    pub index: LeafIndex,
    pub leaf: NullifierLeaf,
}

/// A state together with the writes that must be applied before it is used.
#[derive(Debug)]
pub struct Committed<I> {
    pub imt: I,
    pub writes: WriteBatch,
}

/// A finalized state together with the writes that must be applied before it
/// is used.
#[derive(Debug)]
pub struct Finalized<I> {
    pub imt: I,
    pub writes: WriteBatch,
}

/// A block of the state above the finalized base.
#[derive(Clone, Debug)]
struct BlockRecord {
    version: Version,
    // the length of the tree after the block
    end_len: u64,
    nullifiers: VectorSync<NullifierKey>,
}

/// The block being built: its leaves are staged, its nodes hashed by `seal`.
#[derive(Clone, Debug, Default)]
struct Pending {
    leaves: RedBlackTreeMapSync<LeafIndex, NullifierLeaf>,
    nullifiers: VectorSync<NullifierKey>,
    // the hashed nodes, if no leaf was staged since they were hashed
    sealed: Option<Sealed>,
}

#[derive(Clone, Debug)]
struct Sealed {
    nodes: HashTrieMapSync<NodePosition, PendingNode>,
    // the committed nodes the block replaces
    replaced: VectorSync<NodeKey>,
    root: Fr,
}

#[derive(Clone, Copy, Debug)]
enum PendingChild {
    Committed(Child),
    // written by the pending block
    Pending { hash: Fr },
}

impl PendingChild {
    const fn hash(&self) -> Fr {
        match self {
            Self::Committed(child) => child.hash,
            Self::Pending { hash } => *hash,
        }
    }

    const fn stored(&self, version: Version) -> Child {
        match self {
            Self::Committed(child) => *child,
            Self::Pending { hash } => Child {
                hash: *hash,
                version: Some(version),
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PendingNode {
    Internal {
        left: PendingChild,
        right: PendingChild,
    },
    Leaf {
        leaf: NullifierLeaf,
    },
}

/// Where a walk down the tree is: in the nodes of the sealed block, or in the
/// committed nodes below `Child`.
#[derive(Clone, Copy, Debug)]
enum Cursor {
    Pending,
    Committed(Child),
}

#[derive(Clone, Copy, Debug)]
struct ChildView {
    hash: Fr,
    cursor: Cursor,
}

#[derive(Clone, Copy, Debug)]
struct ChildViews {
    left: ChildView,
    right: ChildView,
}

#[derive(Clone, Copy, Debug)]
struct IndexedNullifier {
    key: NullifierKey,
    entry: IndexEntry,
}

#[derive(Clone, Copy, Debug)]
struct ChildPair {
    left: Child,
    right: Child,
}

/// An indexed Merkle tree of nullifiers of depth `HEIGHT`, kept in `S`.
///
/// The leaves, appended in insertion order, form a linked list sorted by
/// nullifier. The first leaf is the sentinel `(0, 0, 0)` and positions past the
/// last leaf hold 0. Cloning a state is O(1).
pub struct Imt<S, H, const HEIGHT: usize> {
    store: S,
    root: Child,
    len: u64,
    // the nullifiers inserted since the finalized base, the pending block's included
    index: RedBlackTreeMapSync<NullifierKey, IndexEntry>,
    unfinalized: QueueSync<BlockRecord>,
    pending: Option<Pending>,
    _hash: PhantomData<fn() -> H>,
}

impl<S: Clone, H, const HEIGHT: usize> Clone for Imt<S, H, HEIGHT> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            root: self.root,
            len: self.len,
            index: self.index.clone(),
            unfinalized: self.unfinalized.clone(),
            pending: self.pending.clone(),
            _hash: PhantomData,
        }
    }
}

impl<S, H, const HEIGHT: usize> fmt::Debug for Imt<S, H, HEIGHT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Imt")
            .field("root", &self.root)
            .field("len", &self.len)
            .field("unfinalized", &self.unfinalized.len())
            .field("pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

#[expect(
    clippy::len_without_is_empty,
    reason = "the sentinel leaf makes the tree never empty"
)]
impl<S, H, const HEIGHT: usize> Imt<S, H, HEIGHT>
where
    S: KvStore + Clone,
    H: Digest,
{
    const CAPACITY: u64 = 1 << HEIGHT;

    fn empty_root(level: u8) -> Fr {
        UtxoMerkleHasher::<H>::empty_subtree_root(usize::from(level))
    }

    fn empty_child(level: u8) -> Child {
        Child {
            hash: Self::empty_root(level),
            version: None,
        }
    }

    fn compress(left: Fr, right: Fr) -> Fr {
        H::compress(&[left, right])
    }

    const fn root_position() -> NodePosition {
        NodePosition::root(HEIGHT)
    }

    fn base_state(store: S, meta: BaseMeta) -> Self {
        Self {
            store,
            root: meta.root,
            len: meta.len,
            index: RedBlackTreeMapSync::new_sync(),
            unfinalized: QueueSync::new_sync(),
            pending: None,
            _hash: PhantomData,
        }
    }

    fn read_meta(store: &S) -> Result<Option<BaseMeta>, ImtError> {
        store
            .get(&MetaKey.encode())
            .map_err(ImtError::storage)?
            .map(|bytes| {
                BaseMeta::decode(&bytes).map_err(|error| ImtError::corrupted("base meta", &error))
            })
            .transpose()
    }

    /// Writes the genesis tree, holding the sentinel leaf only, as the
    /// finalized state of `block_id`.
    pub fn initialize(store: S, block_id: BlockId) -> Result<Self, ImtError> {
        if Self::read_meta(&store)?.is_some() {
            return Err(ImtError::AlreadyInitialized);
        }
        let empty = Self::base_state(
            store,
            BaseMeta {
                len: 0,
                root: Self::empty_child(HEIGHT as u8),
                block_id,
            },
        );
        let sentinel = empty.stage(LeafIndex::new(0), NullifierLeaf::SENTINEL, None);
        let Committed {
            imt,
            writes: mut batch,
        } = sentinel.commit(Version::GENESIS)?;
        let Finalized { imt, writes } = imt.finalize(block_id)?;
        batch.append(writes);
        imt.store.write_batch(batch).map_err(ImtError::storage)?;
        let garbage = Self::collect_garbage(&imt.store, imt.len)?;
        imt.store.write_batch(garbage).map_err(ImtError::storage)?;
        Ok(imt)
    }

    /// The finalized state of the tree in `store`.
    pub fn open(store: S) -> Result<Self, ImtError> {
        let meta = Self::read_meta(&store)?.ok_or(ImtError::NotInitialized)?;
        Ok(Self::base_state(store, meta))
    }

    /// The finalized base of `store`.
    pub fn base_meta(store: &S) -> Result<BaseMeta, ImtError> {
        Self::read_meta(store)?.ok_or(ImtError::NotInitialized)
    }

    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// Whether the state has no block above the finalized base.
    #[must_use]
    pub fn is_finalized(&self) -> bool {
        self.pending.is_none() && self.unfinalized.is_empty()
    }

    /// Whether the state has a block that is not committed.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The versions of the committed blocks of the state above the finalized
    /// base, oldest first.
    pub fn unfinalized_versions(&self) -> impl Iterator<Item = Version> + '_ {
        self.unfinalized.iter().map(|record| record.version)
    }

    /// The committed root, as stored, or `None` while a block is pending.
    #[must_use]
    pub const fn committed_root(&self) -> Option<Child> {
        if self.pending.is_some() {
            None
        } else {
            Some(self.root)
        }
    }

    /// Whether `nullifier` is in the tree.
    ///
    /// The base index may hold nullifiers inserted after this state, when it is
    /// a canonical state below the finalized base: they are past its `len`.
    pub fn contains(&self, nullifier: &Fr) -> Result<bool, ImtError> {
        let key = NullifierKey {
            nullifier: *nullifier,
        };
        if self.index.contains_key(&key) {
            return Ok(true);
        }
        let Some(bytes) = self.store.get(&key.encode()).map_err(ImtError::storage)? else {
            return Ok(false);
        };
        let entry = IndexEntry::decode(&bytes)
            .map_err(|error| ImtError::corrupted("index entry", &error))?;
        Ok(entry.leaf_index.as_u64() < self.len)
    }

    // The greatest nullifier of the base index lower than `key` and in this
    // state, skipping those inserted after it.
    fn base_predecessor(&self, key: NullifierKey) -> Result<Option<IndexedNullifier>, ImtError> {
        let mut bound = key;
        loop {
            let Some(found) = self
                .store
                .get_prev(&NullifierKey::prefix(), &bound.encode())
                .map_err(ImtError::storage)?
            else {
                return Ok(None);
            };
            let found = IndexedNullifier {
                key: NullifierKey::decode(&found.key)
                    .map_err(|error| ImtError::corrupted("nullifier key", &error))?,
                entry: IndexEntry::decode(&found.value)
                    .map_err(|error| ImtError::corrupted("index entry", &error))?,
            };
            if found.entry.leaf_index.as_u64() < self.len {
                return Ok(Some(found));
            }
            bound = found.key;
        }
    }

    // The leaf of the greatest nullifier lower than `nullifier`.
    fn predecessor(&self, nullifier: &Fr) -> Result<LeafIndex, ImtError> {
        let key = NullifierKey {
            nullifier: *nullifier,
        };
        let overlay = self
            .index
            .range(..key)
            .next_back()
            .map(|(key, entry)| IndexedNullifier {
                key: *key,
                entry: *entry,
            });
        overlay
            .into_iter()
            .chain(self.base_predecessor(key)?)
            .max_by_key(|found| found.key)
            .map(|found| found.entry.leaf_index)
            .ok_or_else(|| {
                ImtError::Corrupted("no nullifier below, not even the sentinel".to_owned())
            })
    }

    /// Returns the leaf of the greatest nullifier lower than `nullifier`, or
    /// `None` if `nullifier` is in the tree.
    pub fn low_leaf(&self, nullifier: &Fr) -> Result<Option<LowLeaf>, ImtError> {
        if self.contains(nullifier)? {
            return Ok(None);
        }
        let index = self.predecessor(nullifier)?;
        let leaf = self
            .leaf(index)?
            .ok_or_else(|| ImtError::Corrupted(format!("indexed leaf {index:?} is missing")))?;
        Ok(Some(LowLeaf { index, leaf }))
    }

    fn read_node(&self, key: NodeKey) -> Result<StoredNode, ImtError> {
        let bytes = self
            .store
            .get(&key.encode())
            .map_err(ImtError::storage)?
            .ok_or(ImtError::MissingNode(key))?;
        StoredNode::decode(&bytes).map_err(|error| ImtError::corrupted("node", &error))
    }

    // The children of the committed node `child` at `position`.
    fn committed_children(
        &self,
        position: NodePosition,
        child: Child,
    ) -> Result<ChildPair, ImtError> {
        let Some(version) = child.version else {
            let empty = Self::empty_child(position.level - 1);
            return Ok(ChildPair {
                left: empty,
                right: empty,
            });
        };
        let key = NodeKey { version, position };
        match self.read_node(key)? {
            StoredNode::Internal { left, right } => Ok(ChildPair { left, right }),
            StoredNode::Leaf { .. } => Err(ImtError::Corrupted(format!(
                "leaf where an internal node was expected at {key:?}"
            ))),
        }
    }

    fn children(
        &self,
        sealed: Option<&Sealed>,
        position: NodePosition,
        cursor: Cursor,
    ) -> Result<ChildViews, ImtError> {
        let view = |child: PendingChild| ChildView {
            hash: child.hash(),
            cursor: match child {
                PendingChild::Committed(child) => Cursor::Committed(child),
                PendingChild::Pending { .. } => Cursor::Pending,
            },
        };
        match cursor {
            Cursor::Pending => match sealed.and_then(|sealed| sealed.nodes.get(&position)) {
                Some(PendingNode::Internal { left, right }) => Ok(ChildViews {
                    left: view(*left),
                    right: view(*right),
                }),
                _ => Err(ImtError::Corrupted(format!(
                    "no pending internal node at {position:?}"
                ))),
            },
            Cursor::Committed(child) => {
                let pair = self.committed_children(position, child)?;
                Ok(ChildViews {
                    left: view(PendingChild::Committed(pair.left)),
                    right: view(PendingChild::Committed(pair.right)),
                })
            }
        }
    }

    fn root_cursor(&self, sealed: Option<&Sealed>) -> Cursor {
        match sealed {
            Some(sealed) if sealed.nodes.contains_key(&Self::root_position()) => Cursor::Pending,
            _ => Cursor::Committed(self.root),
        }
    }

    /// The leaf at `index`, or `None` past the last leaf.
    pub fn leaf(&self, index: LeafIndex) -> Result<Option<NullifierLeaf>, ImtError> {
        if index.as_u64() >= self.len {
            return Ok(None);
        }
        if let Some(leaf) = self
            .pending
            .as_ref()
            .and_then(|pending| pending.leaves.get(&index))
        {
            return Ok(Some(*leaf));
        }
        // a leaf the pending block did not stage is the committed one
        let target = NodePosition::leaf(index);
        let mut position = Self::root_position();
        let mut child = self.root;
        while position.level > 0 {
            let pair = self.committed_children(position, child)?;
            position = position.child_towards(target);
            child = if position.is_right_child() {
                pair.right
            } else {
                pair.left
            };
        }
        let version = child
            .version
            .ok_or_else(|| ImtError::Corrupted(format!("leaf {index:?} is empty")))?;
        let key = NodeKey {
            version,
            position: target,
        };
        match self.read_node(key)? {
            StoredNode::Leaf { leaf } => Ok(Some(leaf)),
            StoredNode::Internal { .. } => Err(ImtError::Corrupted(format!(
                "internal node where a leaf was expected at {key:?}"
            ))),
        }
    }

    // The sealed nodes of the pending block, hashed now if they are not yet.
    fn current_seal(&self) -> Result<Option<Sealed>, ImtError> {
        match &self.pending {
            None => Ok(None),
            Some(Pending {
                sealed: Some(sealed),
                ..
            }) => Ok(Some(sealed.clone())),
            Some(pending) => self.compute_seal(pending).map(Some),
        }
    }

    /// The root of the tree, the pending block's leaves included. While the
    /// block is not sealed, this hashes it without keeping the result.
    pub fn root(&self) -> Result<Fr, ImtError> {
        Ok(self
            .current_seal()?
            .map_or(self.root.hash, |sealed| sealed.root))
    }

    /// The siblings from the leaf at `index` up to the root, or `None` past
    /// the last leaf. While the pending block is not sealed, this hashes it
    /// without keeping the result.
    pub fn path(&self, index: LeafIndex) -> Result<Option<MerklePath<Fr, HEIGHT>>, ImtError> {
        if index.as_u64() >= self.len {
            return Ok(None);
        }
        let sealed = self.current_seal()?;
        let sealed = sealed.as_ref();
        let target = NodePosition::leaf(index);
        let mut position = Self::root_position();
        let mut cursor = self.root_cursor(sealed);
        let mut siblings = Vec::with_capacity(HEIGHT);
        while position.level > 0 {
            let children = self.children(sealed, position, cursor)?;
            position = position.child_towards(target);
            if position.is_right_child() {
                siblings.push(MerkleNode::Left(children.left.hash));
                cursor = children.right.cursor;
            } else {
                siblings.push(MerkleNode::Right(children.right.hash));
                cursor = children.left.cursor;
            }
        }
        siblings.reverse();
        Ok(Some(siblings.try_into().map_err(|_| {
            ImtError::Corrupted("path of the wrong length".to_owned())
        })?))
    }

    fn stage(&self, index: LeafIndex, leaf: NullifierLeaf, low: Option<LowLeaf>) -> Self {
        let mut pending = self.pending.clone().unwrap_or_default();
        if let Some(low) = low {
            pending.leaves.insert_mut(low.index, low.leaf);
        }
        let key = NullifierKey {
            nullifier: leaf.nullifier,
        };
        pending.leaves.insert_mut(index, leaf);
        pending.nullifiers.push_back_mut(key);
        pending.sealed = None;
        Self {
            store: self.store.clone(),
            root: self.root,
            len: self.len + 1,
            index: self.index.insert(key, IndexEntry { leaf_index: index }),
            unfinalized: self.unfinalized.clone(),
            pending: Some(pending),
            _hash: PhantomData,
        }
    }

    /// Inserts `nullifier` in the pending block. Its hashes are computed by
    /// [`Self::seal`].
    pub fn insert(&self, nullifier: Fr) -> Result<Self, ImtError> {
        if nullifier == Fr::ZERO {
            return Err(ImtError::ZeroNullifier);
        }
        let low = self
            .low_leaf(&nullifier)?
            .ok_or(ImtError::AlreadyPresent(nullifier))?;
        if self.len == Self::CAPACITY {
            return Err(ImtError::Full);
        }
        let index = LeafIndex::new(self.len);
        let leaf = NullifierLeaf {
            nullifier,
            next_nullifier: low.leaf.next_nullifier,
            next_index: low.leaf.next_index,
        };
        let low = LowLeaf {
            index: low.index,
            leaf: NullifierLeaf {
                next_nullifier: nullifier,
                next_index: index,
                ..low.leaf
            },
        };
        Ok(self.stage(index, leaf, Some(low)))
    }

    /// Hashes the leaves staged by the pending block, once each node.
    pub fn seal(&self) -> Result<Self, ImtError> {
        let Some(pending) = &self.pending else {
            return Ok(self.clone());
        };
        if pending.sealed.is_some() {
            return Ok(self.clone());
        }
        let sealed = self.compute_seal(pending)?;
        Ok(Self {
            pending: Some(Pending {
                sealed: Some(sealed),
                ..pending.clone()
            }),
            ..self.clone()
        })
    }

    fn compute_seal(&self, pending: &Pending) -> Result<Sealed, ImtError> {
        // the positions the block writes, level by level from the leaves
        let mut dirty: Vec<BTreeSet<u64>> = Vec::with_capacity(HEIGHT + 1);
        dirty.push(pending.leaves.keys().map(|index| index.as_u64()).collect());
        for level in 1..=HEIGHT {
            let below = &dirty[level - 1];
            let parents = below.iter().map(|index| index >> 1).collect();
            dirty.push(parents);
        }

        // the committed child at each dirty position and at their siblings,
        // reading each dirty committed node once, from the root down
        let mut committed: HashMap<NodePosition, Child> = HashMap::new();
        committed.insert(Self::root_position(), self.root);
        for level in (1..=HEIGHT).rev() {
            for index in &dirty[level] {
                let position = NodePosition {
                    level: level as u8,
                    index: *index,
                };
                let child = committed[&position];
                let pair = self.committed_children(position, child)?;
                committed.insert(position.left(), pair.left);
                committed.insert(position.right(), pair.right);
            }
        }

        let mut nodes = HashTrieMapSync::new_sync();
        let mut replaced = VectorSync::new_sync();
        let mut hashes: HashMap<NodePosition, Fr> = HashMap::new();
        for (level, indices) in dirty.iter().enumerate() {
            for index in indices {
                let position = NodePosition {
                    level: level as u8,
                    index: *index,
                };
                if let Some(version) = committed[&position].version {
                    replaced.push_back_mut(NodeKey { version, position });
                }
                let (node, hash) = if level == 0 {
                    let leaf = pending.leaves[&LeafIndex::new(*index)];
                    (PendingNode::Leaf { leaf }, leaf.hash::<H>())
                } else {
                    let child = |position: NodePosition| {
                        hashes.get(&position).map_or_else(
                            || PendingChild::Committed(committed[&position]),
                            |hash| PendingChild::Pending { hash: *hash },
                        )
                    };
                    let left = child(position.left());
                    let right = child(position.right());
                    (
                        PendingNode::Internal { left, right },
                        Self::compress(left.hash(), right.hash()),
                    )
                };
                nodes.insert_mut(position, node);
                hashes.insert(position, hash);
            }
        }
        let root = hashes
            .get(&Self::root_position())
            .copied()
            .unwrap_or(self.root.hash);
        Ok(Sealed {
            nodes,
            replaced,
            root,
        })
    }

    /// Writes the pending block under `version`: its nodes, and a stale record
    /// of each committed node it replaces. A state without a pending block is
    /// returned as is, without writes.
    pub fn commit(&self, version: Version) -> Result<Committed<Self>, ImtError> {
        let Some(pending) = &self.pending else {
            return Ok(Committed {
                imt: self.clone(),
                writes: WriteBatch::new(),
            });
        };
        let sealed = match &pending.sealed {
            Some(sealed) => sealed.clone(),
            None => self.compute_seal(pending)?,
        };
        let mut writes = WriteBatch::new();
        for (position, node) in &sealed.nodes {
            let stored = match node {
                PendingNode::Internal { left, right } => StoredNode::Internal {
                    left: left.stored(version),
                    right: right.stored(version),
                },
                PendingNode::Leaf { leaf } => StoredNode::Leaf { leaf: *leaf },
            };
            let key = NodeKey {
                version,
                position: *position,
            };
            writes.put(key.encode(), stored.encode());
        }
        for node in &sealed.replaced {
            let key = StaleKey {
                replaced_by: version,
                node: *node,
            };
            writes.put(key.encode(), StaleMarker.encode());
        }
        let root = if sealed.nodes.contains_key(&Self::root_position()) {
            Child {
                hash: sealed.root,
                version: Some(version),
            }
        } else {
            self.root
        };
        Ok(Committed {
            imt: Self {
                store: self.store.clone(),
                root,
                len: self.len,
                index: self.index.clone(),
                unfinalized: self.unfinalized.enqueue(BlockRecord {
                    version,
                    end_len: self.len,
                    nullifiers: pending.nullifiers.clone(),
                }),
                pending: None,
                _hash: PhantomData,
            },
            writes,
        })
    }

    /// Makes this state, of block `block_id`, the finalized base: moves the
    /// nullifiers of its blocks above the base to the base index, and queues
    /// the nodes they replaced for [`Self::collect_garbage`]. Every state that
    /// is not a canonical one, an ancestor or a descendant of this one, becomes
    /// invalid.
    pub fn finalize(&self, block_id: BlockId) -> Result<Finalized<Self>, ImtError> {
        if self.pending.is_some() {
            return Err(ImtError::UncommittedBlock);
        }
        let mut writes = WriteBatch::new();
        for record in &self.unfinalized {
            let gc = GcKey {
                end_len: record.end_len,
                version: record.version,
            };
            writes.put(gc.encode(), GcMarker.encode());
            for key in &record.nullifiers {
                let entry = self.index.get(key).ok_or_else(|| {
                    ImtError::Corrupted(format!("nullifier {key:?} of a block is not indexed"))
                })?;
                writes.put(key.encode(), entry.encode());
            }
        }
        let meta = BaseMeta {
            len: self.len,
            root: self.root,
            block_id,
        };
        writes.put(MetaKey.encode(), meta.encode());
        Ok(Finalized {
            imt: Self::base_state(self.store.clone(), meta),
            writes,
        })
    }

    /// The writes deleting the nodes replaced by the finalized blocks that
    /// every retained state has reached: those after which the tree had at
    /// most `floor_len` leaves, `floor_len` being the length of the oldest
    /// canonical state still read (the finalized base, or an older snapshot).
    pub fn collect_garbage(store: &S, floor_len: u64) -> Result<WriteBatch, ImtError> {
        let mut writes = WriteBatch::new();
        for entry in store
            .scan_prefix(&GcKey::prefix())
            .map_err(ImtError::storage)?
        {
            let gc =
                GcKey::decode(&entry.key).map_err(|error| ImtError::corrupted("gc key", &error))?;
            // the keys are ordered by `end_len`
            if gc.end_len > floor_len {
                break;
            }
            let prefix = StaleKey::version_prefix(gc.version);
            for stale in store.scan_prefix(&prefix).map_err(ImtError::storage)? {
                let stale = StaleKey::decode(&stale.key)
                    .map_err(|error| ImtError::corrupted("stale key", &error))?;
                writes.delete(stale.node.encode());
            }
            writes.delete_prefix(prefix).delete(entry.key);
        }
        Ok(writes)
    }

    /// The state as its length and root, enough to read it again with
    /// [`Self::from_snapshot`] once it is a canonical state at or below the
    /// finalized base.
    pub const fn snapshot(&self) -> Result<Snapshot, ImtError> {
        if self.pending.is_some() {
            return Err(ImtError::UncommittedBlock);
        }
        Ok(Snapshot {
            len: self.len,
            root: self.root,
        })
    }

    /// A canonical state at or below the finalized base, from its snapshot.
    pub fn from_snapshot(store: S, snapshot: Snapshot) -> Result<Self, ImtError> {
        let meta = Self::base_meta(&store)?;
        if snapshot.len > meta.len {
            return Err(ImtError::BaseMismatch);
        }
        Ok(Self::base_state(
            store,
            BaseMeta {
                len: snapshot.len,
                root: snapshot.root,
                block_id: meta.block_id,
            },
        ))
    }

    /// Drops from memory the blocks of the state the finalized base of the
    /// store holds, so that the memory of a state follows the base. It only
    /// frees memory: the dropped nullifiers are in the base index.
    pub fn rebase(&self) -> Result<Self, ImtError> {
        let meta = Self::base_meta(&self.store)?;
        let mut unfinalized = self.unfinalized.clone();
        let mut index = self.index.clone();
        while let Some(record) = unfinalized.peek() {
            if record.end_len > meta.len {
                break;
            }
            for key in &record.nullifiers {
                index.remove_mut(key);
            }
            unfinalized.dequeue_mut();
        }
        Ok(Self {
            index,
            unfinalized,
            ..self.clone()
        })
    }

    /// The writes deleting everything the block of `version` wrote, once its
    /// fork is pruned. The nodes it replaced are kept: they belong to its
    /// ancestors.
    #[must_use]
    pub fn discard_writes(version: Version) -> WriteBatch {
        let mut writes = WriteBatch::new();
        writes
            .delete_prefix(NodeKey::version_prefix(version))
            .delete_prefix(StaleKey::version_prefix(version));
        writes
    }

    /// Checks that this finalized state is the base of the store.
    pub fn check_base(&self) -> Result<(), ImtError> {
        let meta = Self::base_meta(&self.store)?;
        if !self.is_finalized() || meta.len != self.len || meta.root != self.root {
            return Err(ImtError::BaseMismatch);
        }
        Ok(())
    }
}
