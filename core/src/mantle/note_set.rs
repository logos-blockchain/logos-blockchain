use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt,
    ops::Deref,
    str::FromStr,
    sync::{Arc, LazyLock},
};

use lb_dynamic_merkle::{DynamicMerkleTree, MerklePath};
use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes, serde::serde_fr};
use lb_imt::{
    IndexedMerkleTree, NullifierLeaf,
    store::{BlockId, Committed, Finalized, Imt, ImtError, Version},
};
use lb_kv_store::{Bytes, DynKvStore, ErasedKv, MemKv, Prefixed, WriteBatch};
use lb_mmr::MerkleMountainRange;
use lb_poseidon2::Digest as _;
use lb_utxotree::UtxoMerkleHasher;
use rpds::{HashTrieMapSync, HashTrieSetSync, RedBlackTreeSetSync, VectorSync};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{EnumAccess, Error as _, Unexpected, VariantAccess as _, Visitor},
    ser::Error as _,
};
use thiserror::Error;

use crate::{
    block::BlockNumber,
    crypto::{Digest as _, Hasher, ZkHasher},
    mantle::{
        notes::{NoteCommitment, NoteNullifier},
        ops::channel::ChannelId,
    },
};

/// A note set of the ledger: the ledger set, the SDP set or the set of a
/// channel.
///
/// A human-readable format writes it as a string, `ledger`, `sdp` or
/// `channel:<hex channel id>`, so it can key a JSON map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteSetIndex {
    Ledger,
    Sdp,
    Channel(ChannelId),
}

const CHANNEL_PREFIX: &str = "channel:";

impl fmt::Display for NoteSetIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ledger => f.write_str("ledger"),
            Self::Sdp => f.write_str("sdp"),
            Self::Channel(channel_id) => write!(f, "{CHANNEL_PREFIX}{channel_id}"),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0:?} is not `ledger`, `sdp` or `channel:<hex channel id>`")]
pub struct ParseNoteSetIndexError(String);

impl FromStr for NoteSetIndex {
    type Err = ParseNoteSetIndexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let error = || ParseNoteSetIndexError(s.to_owned());
        match s {
            "ledger" => Ok(Self::Ledger),
            "sdp" => Ok(Self::Sdp),
            _ => {
                let hex_id = s.strip_prefix(CHANNEL_PREFIX).ok_or_else(error)?;
                let mut bytes = [0u8; 32];
                hex::decode_to_slice(hex_id, &mut bytes).map_err(|_| error())?;
                Ok(Self::Channel(ChannelId::from(bytes)))
            }
        }
    }
}

const NOTE_SET_INDEX: &str = "NoteSetIndex";
const NOTE_SET_INDEX_VARIANTS: &[&str] = &["Ledger", "Sdp", "Channel"];

// A binary format encodes the index as an enum.
impl Serialize for NoteSetIndex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            return serializer.collect_str(self);
        }
        match self {
            Self::Ledger => serializer.serialize_unit_variant(NOTE_SET_INDEX, 0, "Ledger"),
            Self::Sdp => serializer.serialize_unit_variant(NOTE_SET_INDEX, 1, "Sdp"),
            Self::Channel(channel_id) => {
                serializer.serialize_newtype_variant(NOTE_SET_INDEX, 2, "Channel", channel_id)
            }
        }
    }
}

impl<'de> Deserialize<'de> for NoteSetIndex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            return String::deserialize(deserializer)?
                .parse()
                .map_err(D::Error::custom);
        }
        deserializer.deserialize_enum(NOTE_SET_INDEX, NOTE_SET_INDEX_VARIANTS, NoteSetIndexVisitor)
    }
}

struct NoteSetIndexVisitor;

impl<'de> Visitor<'de> for NoteSetIndexVisitor {
    type Value = NoteSetIndex;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a note set index")
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
        let (variant, access) = data.variant::<u32>()?;
        match variant {
            0 => access.unit_variant().map(|()| NoteSetIndex::Ledger),
            1 => access.unit_variant().map(|()| NoteSetIndex::Sdp),
            2 => access.newtype_variant().map(NoteSetIndex::Channel),
            _ => Err(A::Error::invalid_value(
                Unexpected::Unsigned(variant.into()),
                &"a variant index 0 <= i < 3",
            )),
        }
    }
}

// The leaves of the eligible tree: the ledger set, the SDP set, then the
// channel sets in creation order.
const LEDGER_LEAF: usize = 0;
const SDP_LEAF: usize = 1;
const FIRST_CHANNEL_LEAF: usize = 2;

// A root can be referenced while validating the 1024 blocks after the last
// block it was current at.
pub const RECENT_ROOTS_WINDOW: BlockNumber = 1024;

pub type CommitmentMmr = MerkleMountainRange<NoteCommitment, ZkHasher, 41>;
/// The store of a note set: its namespace in the store of its [`NoteSets`].
pub type NoteSetStore = Prefixed<DynKvStore>;
pub type NullifierImt = Imt<NoteSetStore, ZkHasher, 40>;
// The tree as serialized, checked when decoded.
type CheckedNullifierImt = IndexedMerkleTree<ZkHasher, 40>;
type EligibleTree = DynamicMerkleTree<UtxoMerkleHasher<ZkHasher>, 40>;

static ELIGIBLE_SET_LEAF_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"ELIGIBLE_SET_LEAF_V1").expect("BigUint should load from constant string")
});

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum NoteSetError {
    #[error("Inputs cannot be empty")]
    EmptyInputs,
    #[error("Inputs contain the nullifier {0:?} twice")]
    DuplicateInput(NoteNullifier),
    #[error("Commitment root {0} is not a recent root of the note set")]
    UnknownRoot(Fr),
    #[error("Note of nullifier {0:?} is already spent")]
    AlreadySpent(NoteNullifier),
    #[error("Cannot append {buffered} buffered commitments out of {len}")]
    BufferedTooLarge { buffered: usize, len: usize },
    #[error(transparent)]
    Imt(#[from] ImtError),
    #[error("The commitment MMR is full")]
    MmrFull,
}

// The commitment MMR of a referenced root extended by a prefix of the
// transaction buffer is kept every `PREFIX_CHECKPOINT` commitments, so each
// root replays the buffer once and each lookup at most that many commitments.
const PREFIX_CHECKPOINT: usize = 64;

// Chained hash of the buffer prefix: with the root and the length of the
// referenced MMR (a frontier root does not tell trailing zero leaves apart), it
// names the extended MMR whatever note set or clone computed it.
type PrefixId = [u8; 32];

const EMPTY_PREFIX: PrefixId = [0; 32];

type TxRootKey = (Fr, usize, PrefixId);

// The extended MMRs computed during a Mantle Transaction, shared by its
// Operations. The verification of an Operation only reads the state it is
// verified against, so the cache fills behind a shared reference.
#[derive(Default)]
pub struct TxRoots(RefCell<HashMap<TxRootKey, ExtendedMmr>>);

// An extended MMR and, once a proof named it, its root. A checkpoint on the way
// to another prefix has none.
#[derive(Clone)]
struct ExtendedMmr {
    mmr: CommitmentMmr,
    root: Option<Fr>,
}

impl fmt::Debug for TxRoots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TxRoots")
    }
}

/// The commitments the previous Operations of a Mantle Transaction added to a
/// note set, which the next ones are proven against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxBuffer {
    commitments: Vec<NoteCommitment>,
    // the id of each prefix of `commitments`, ending at each commitment
    prefix_ids: Vec<PrefixId>,
}

impl TxBuffer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            commitments: Vec::new(),
            prefix_ids: Vec::new(),
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.commitments.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.commitments.is_empty()
    }

    pub fn push(&mut self, outputs: &[NoteCommitment]) {
        for note_commitment in outputs {
            let previous = self.prefix_ids.last().copied().unwrap_or(EMPTY_PREFIX);
            let prefix_id: PrefixId = Hasher::new()
                .chain_update(previous)
                .chain_update(fr_to_bytes(&note_commitment.0))
                .finalize()
                .into();
            self.commitments.push(*note_commitment);
            self.prefix_ids.push(prefix_id);
        }
    }

    #[must_use]
    pub fn with(mut self, outputs: &[NoteCommitment]) -> Self {
        self.push(outputs);
        self
    }

    // The id of the first `len` commitments.
    fn prefix_id(&self, len: usize) -> PrefixId {
        len.checked_sub(1)
            .map_or(EMPTY_PREFIX, |last| self.prefix_ids[last])
    }

    const fn check_buffered(&self, buffered: usize) -> Result<(), NoteSetError> {
        if buffered > self.len() {
            return Err(NoteSetError::BufferedTooLarge {
                buffered,
                len: self.len(),
            });
        }
        Ok(())
    }
}

/// What the Operations of a Mantle Transaction share about the note sets.
///
/// It holds the buffer of each set they added to and the roots computed from
/// them. Created empty for every transaction and dropped with it, it is never
/// ledger state.
#[derive(Debug, Default)]
pub struct TxNoteState {
    buffers: HashMap<NoteSetIndex, TxBuffer>,
    roots: TxRoots,
}

impl TxNoteState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The buffer of the set `index`, empty if no Operation added to it.
    #[must_use]
    pub fn buffer(&self, index: NoteSetIndex) -> Cow<'_, TxBuffer> {
        self.buffers
            .get(&index)
            .map_or_else(|| Cow::Owned(TxBuffer::new()), Cow::Borrowed)
    }

    /// Adds `outputs` to the buffer of the set `index`, which the next
    /// Operations of the transaction are proven against.
    pub fn buffer_outputs(&mut self, index: NoteSetIndex, outputs: &[NoteCommitment]) {
        if !outputs.is_empty() {
            self.buffers.entry(index).or_default().push(outputs);
        }
    }

    #[must_use]
    pub const fn roots(&self) -> &TxRoots {
        &self.roots
    }

    /// Checks that `inputs` are spendable in `set` and returns the root a
    /// `ZkTransfer` spending them is proven against: `commitment_merkle_root`
    /// extended with the buffer of the set.
    pub fn spendable_root<K: SetKind>(
        &self,
        set: &TypedNoteSet<K>,
        inputs: &[NoteNullifier],
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        set.assert_spendable(inputs)?;
        self.commitment_root(set, commitment_merkle_root, block)
    }

    /// `commitment_merkle_root` of `set` extended with its buffer.
    pub fn commitment_root<K: SetKind>(
        &self,
        set: &TypedNoteSet<K>,
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        let buffer = self.buffer(set.index());
        set.cached_step_commitment_root(
            &self.roots,
            &buffer,
            commitment_merkle_root,
            buffer.len(),
            block,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSetUpdate {
    pub note_set: NoteSetIndex,
    // inserted during the block, in execution order
    pub nullifiers: Vec<NoteNullifier>,
    // appended during the block, in execution order
    pub commitments: Vec<NoteCommitment>,
}

/// A note set: the commitments of its notes and the nullifiers of the spent
/// ones.
///
/// The nullifier tree lives in the store of the set (see [`NoteSets`]): a set
/// holds in memory only what its blocks above the finalized base inserted.
/// A block inserts into a pending block of the tree, sealed at the end of the
/// block and written by [`NoteSets::commit`].
#[derive(Clone, Debug, PartialEq)]
pub struct NoteSet {
    commitments: CommitmentMmr,
    nullifiers: NullifierImt,
    // root at the end of the last block that changed the set
    committed_root: Fr,
    // snapshot of each recent root, with the block at whose end it was superseded
    recent_commitment_roots: RecentRoots,
    // what the set inserted and appended during the current block, reported at its end
    block_nullifiers: VectorSync<NoteNullifier>,
    block_commitments: VectorSync<NoteCommitment>,
}

impl NoteSet {
    /// An empty set, in a store of its own kept in memory.
    #[must_use]
    pub fn empty() -> Self {
        Self::empty_in(Prefixed::new(in_memory_store(), Bytes::new()))
    }

    /// An empty set whose nullifier tree is kept in `store`. Nothing is
    /// written before the first [`NoteSets::commit`] of the set.
    #[must_use]
    pub fn empty_in(store: NoteSetStore) -> Self {
        Self::with_nullifiers(CommitmentMmr::new(), NullifierImt::empty(store))
    }

    fn with_nullifiers(commitments: CommitmentMmr, nullifiers: NullifierImt) -> Self {
        let committed_root = commitments.frontier_root();
        Self {
            recent_commitment_roots: {
                let mut roots = RecentRoots::default();
                roots.insert_mut(committed_root, commitments.clone(), 0);
                roots
            },
            commitments,
            nullifiers,
            committed_root,
            block_nullifiers: VectorSync::new_sync(),
            block_commitments: VectorSync::new_sync(),
        }
    }

    #[must_use]
    pub fn commitment_root(&self) -> Fr {
        self.commitments.frontier_root()
    }

    // The leaf of the set in the eligible tree, between blocks.
    fn eligible_leaf(&self) -> Fr {
        eligible_leaf(&self.commitment_root(), &self.sealed_nullifier_root())
    }

    /// The root of the nullifier tree. It reads nothing between blocks, once
    /// the block is sealed; during a block it hashes the nullifiers the block
    /// inserted so far.
    pub fn nullifier_root(&self) -> Result<Fr, NoteSetError> {
        Ok(self.nullifiers.root()?)
    }

    // The root of the sealed nullifier tree, for the eligible leaf of the set.
    fn sealed_nullifier_root(&self) -> Fr {
        self.nullifiers
            .sealed_root()
            .expect("the nullifier tree of a set is sealed between blocks")
    }

    #[must_use]
    pub const fn commitments(&self) -> &CommitmentMmr {
        &self.commitments
    }

    #[must_use]
    pub const fn nullifiers(&self) -> &NullifierImt {
        &self.nullifiers
    }

    pub fn contains_nullifier(&self, nullifier: &NoteNullifier) -> Result<bool, NoteSetError> {
        Ok(self.nullifiers.contains(&nullifier.0)?)
    }

    /// Whether any of `nullifiers` is spent.
    pub fn contains_any_nullifier(
        &self,
        nullifiers: &[NoteNullifier],
    ) -> Result<bool, NoteSetError> {
        for nullifier in nullifiers {
            if self.contains_nullifier(nullifier)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // The inputs are not empty, distinct and unspent. The root they are proven
    // against is checked recent by `tx_commitment_root`.
    pub fn assert_spendable(&self, inputs: &[NoteNullifier]) -> Result<(), NoteSetError> {
        if inputs.is_empty() {
            return Err(NoteSetError::EmptyInputs);
        }
        let mut unique = HashSet::with_capacity(inputs.len());
        for input in inputs {
            if !unique.insert(input) {
                return Err(NoteSetError::DuplicateInput(*input));
            }
        }
        for input in inputs {
            if self.contains_nullifier(input)? {
                return Err(NoteSetError::AlreadySpent(*input));
            }
        }
        Ok(())
    }

    /// `commitment_merkle_root` extended with all of `buffer`.
    pub fn tx_commitment_root(
        &self,
        buffer: &TxBuffer,
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        self.step_commitment_root(buffer, commitment_merkle_root, buffer.len(), block)
    }

    /// `commitment_merkle_root` extended with the first `buffered` commitments
    /// of `buffer`, replayed one by one.
    pub fn step_commitment_root(
        &self,
        buffer: &TxBuffer,
        commitment_merkle_root: &Fr,
        buffered: usize,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        buffer.check_buffered(buffered)?;
        let base = self.recent_mmr(commitment_merkle_root, block)?;
        if buffered == 0 {
            return Ok(unextended_root(commitment_merkle_root, base));
        }
        let mut mmr = base.clone();
        for note_commitment in buffer.commitments.iter().take(buffered) {
            mmr = mmr
                .push(*note_commitment)
                .map_err(|_| NoteSetError::MmrFull)?;
        }
        Ok(mmr.frontier_root())
    }

    // Same root as `step_commitment_root`, resuming from the closest extended MMR
    // `tx_roots` holds for the referenced root instead of replaying the whole
    // buffer for every Operation and step.
    pub fn cached_step_commitment_root(
        &self,
        tx_roots: &TxRoots,
        buffer: &TxBuffer,
        commitment_merkle_root: &Fr,
        buffered: usize,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        buffer.check_buffered(buffered)?;
        let base = self.recent_mmr(commitment_merkle_root, block)?;
        if buffered == 0 {
            return Ok(unextended_root(commitment_merkle_root, base));
        }
        let key = |len: usize| (*commitment_merkle_root, base.len(), buffer.prefix_id(len));
        let mut cache = tx_roots.0.borrow_mut();
        if let Some(root) = cache.get(&key(buffered)).and_then(|extended| extended.root) {
            return Ok(root);
        }
        let mut start = buffered;
        let mut mmr = loop {
            if let Some(extended) = cache.get(&key(start)) {
                break extended.mmr.clone();
            }
            if start == 0 {
                break base.clone();
            }
            start = (start - 1) / PREFIX_CHECKPOINT * PREFIX_CHECKPOINT;
        };
        for len in start + 1..=buffered {
            mmr = mmr
                .push(buffer.commitments[len - 1])
                .map_err(|_| NoteSetError::MmrFull)?;
            if len % PREFIX_CHECKPOINT == 0 {
                cache.insert(
                    key(len),
                    ExtendedMmr {
                        mmr: mmr.clone(),
                        root: None,
                    },
                );
            }
        }
        let root = mmr.frontier_root();
        cache.insert(
            key(buffered),
            ExtendedMmr {
                mmr,
                root: Some(root),
            },
        );
        drop(cache);
        Ok(root)
    }

    pub fn execute_spending(mut self, inputs: &[NoteNullifier]) -> Result<Self, NoteSetError> {
        for input in inputs {
            self.nullifiers = self.nullifiers.insert(input.0)?;
            self.block_nullifiers.push_back_mut(*input);
        }
        Ok(self)
    }

    pub fn execute_adding(mut self, outputs: &[NoteCommitment]) -> Result<Self, NoteSetError> {
        for output in outputs {
            self.commitments = self
                .commitments
                .push(*output)
                .map_err(|_| NoteSetError::MmrFull)?;
            self.block_commitments.push_back_mut(*output);
        }
        Ok(self)
    }

    // The snapshot of `commitment_merkle_root` if it can be referenced while
    // validating `block`.
    fn recent_mmr(
        &self,
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<&CommitmentMmr, NoteSetError> {
        match self
            .recent_commitment_roots
            .by_root
            .get(commitment_merkle_root)
        {
            Some((mmr, superseded_at))
                if *commitment_merkle_root == self.committed_root
                    || is_recent(*superseded_at, block) =>
            {
                Ok(mmr)
            }
            _ => Err(NoteSetError::UnknownRoot(*commitment_merkle_root)),
        }
    }

    // Called at the end of `block` on a set the block changed. The nullifiers the
    // block inserted are hashed into the tree here, once each.
    fn record_commitment_root(mut self, block: BlockNumber) -> Result<Self, NoteSetError> {
        self.nullifiers = self.nullifiers.seal()?;
        let previous = self.committed_root;
        if let Some((mmr, _)) = self.recent_commitment_roots.by_root.get(&previous).cloned() {
            // The empty seed root is current before block 0, so a root is
            // stored with the block that superseded it.
            self.recent_commitment_roots
                .insert_mut(previous, mmr, block);
        }
        self.committed_root = self.commitment_root();
        self.recent_commitment_roots.insert_mut(
            self.committed_root,
            self.commitments.clone(),
            block,
        );
        self.recent_commitment_roots
            .expire_mut(&self.committed_root, block);
        self.block_nullifiers = VectorSync::new_sync();
        self.block_commitments = VectorSync::new_sync();
        Ok(self)
    }
}

// A recent root is stored with the snapshot whose frontier root it is, so an
// extension by no buffered commitment is the referenced root itself.
fn unextended_root(commitment_merkle_root: &Fr, snapshot: &CommitmentMmr) -> Fr {
    debug_assert_eq!(snapshot.frontier_root(), *commitment_merkle_root);
    *commitment_merkle_root
}

const fn is_recent(superseded_at: BlockNumber, block: BlockNumber) -> bool {
    superseded_at.saturating_add(RECENT_ROOTS_WINDOW) > block
}

// The recent roots of a set, each with its snapshot and the block at whose end
// it was superseded, also ordered by that block so a block end removes the
// expired roots without visiting the others.
#[derive(Clone, Debug, Default)]
struct RecentRoots {
    by_root: HashTrieMapSync<Fr, (CommitmentMmr, BlockNumber)>,
    by_superseded_at: RedBlackTreeSetSync<(BlockNumber, Fr)>,
}

impl RecentRoots {
    fn insert_mut(&mut self, root: Fr, mmr: CommitmentMmr, superseded_at: BlockNumber) {
        if let Some((_, previous)) = self.by_root.get(&root) {
            self.by_superseded_at.remove_mut(&(*previous, root));
        }
        self.by_root.insert_mut(root, (mmr, superseded_at));
        self.by_superseded_at.insert_mut((superseded_at, root));
    }

    // Removes the roots other than `current` that cannot be referenced after
    // `block`. Once a root of the order by block is recent the later ones are
    // too, so the scan stops at the first one.
    fn expire_mut(&mut self, current: &Fr, block: BlockNumber) {
        let expired: Vec<(BlockNumber, Fr)> = self
            .by_superseded_at
            .iter()
            .take_while(|(superseded_at, _)| !is_recent(*superseded_at, block))
            .filter(|(_, root)| root != current)
            .copied()
            .collect();
        for entry in &expired {
            self.by_root.remove_mut(&entry.1);
            self.by_superseded_at.remove_mut(entry);
        }
    }
}

// The order by block is derived from the roots.
impl PartialEq for RecentRoots {
    fn eq(&self, other: &Self) -> bool {
        self.by_root == other.by_root
    }
}

impl Eq for RecentRoots {}

/// The kind of a [`TypedNoteSet`], which names the set.
pub trait SetKind {
    fn index(&self) -> NoteSetIndex;
}

/// The kind of the ledger note set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ledger;

/// The kind of the SDP note set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sdp;

/// The kind of the note set of a channel, at its leaf of the eligible tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channel {
    id: ChannelId,
    leaf: usize,
}

impl Channel {
    #[must_use]
    pub const fn id(&self) -> ChannelId {
        self.id
    }
}

impl SetKind for Ledger {
    fn index(&self) -> NoteSetIndex {
        NoteSetIndex::Ledger
    }
}

impl SetKind for Sdp {
    fn index(&self) -> NoteSetIndex {
        NoteSetIndex::Sdp
    }
}

impl SetKind for Channel {
    fn index(&self) -> NoteSetIndex {
        NoteSetIndex::Channel(self.id)
    }
}

/// A note set that knows which set of [`NoteSets`] it is, so it cannot be
/// taken for another one. Spending from or adding to it keeps its kind.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedNoteSet<K> {
    kind: K,
    set: NoteSet,
}

pub type LedgerNoteSet = TypedNoteSet<Ledger>;
pub type SdpNoteSet = TypedNoteSet<Sdp>;
pub type ChannelNoteSet = TypedNoteSet<Channel>;

impl<K: SetKind> TypedNoteSet<K> {
    #[must_use]
    pub fn index(&self) -> NoteSetIndex {
        self.kind.index()
    }

    pub fn execute_spending(self, inputs: &[NoteNullifier]) -> Result<Self, NoteSetError> {
        Ok(Self {
            set: self.set.execute_spending(inputs)?,
            ..self
        })
    }

    pub fn execute_adding(self, outputs: &[NoteCommitment]) -> Result<Self, NoteSetError> {
        Ok(Self {
            set: self.set.execute_adding(outputs)?,
            ..self
        })
    }

    #[must_use]
    pub fn into_inner(self) -> NoteSet {
        self.set
    }
}

impl<K> Deref for TypedNoteSet<K> {
    type Target = NoteSet;

    fn deref(&self) -> &NoteSet {
        &self.set
    }
}

/// The note sets of a ledger state, and their eligible tree.
///
/// The nullifier trees of the sets are kept in one store, each set in its own
/// namespace, named by the set (see [`note_set_namespace`]). A set created by a
/// block, such as a channel set, starts in the store with that block: two forks
/// can create the same channel, the blocks of each fork being written under
/// their own versions.
#[derive(Clone)]
pub struct NoteSets {
    store: DynKvStore,
    ledger: LedgerNoteSet,
    sdp: SdpNoteSet,
    channels: HashTrieMapSync<ChannelId, ChannelNoteSet>,
    eligible: EligibleTree,
    // sets whose notes moved in the current block
    changed: HashTrieSetSync<NoteSetIndex>,
}

impl fmt::Debug for NoteSets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoteSets")
            .field("ledger", &self.ledger)
            .field("sdp", &self.sdp)
            .field("channels", &self.channels)
            .field("eligible", &self.eligible)
            .field("changed", &self.changed)
            .finish_non_exhaustive()
    }
}

// The store is where the sets are kept, not what they hold.
impl PartialEq for NoteSets {
    fn eq(&self, other: &Self) -> bool {
        self.ledger == other.ledger
            && self.sdp == other.sdp
            && self.channels == other.channels
            && self.eligible == other.eligible
            && self.changed == other.changed
    }
}

impl Default for NoteSets {
    fn default() -> Self {
        Self::new()
    }
}

const LEDGER_NAMESPACE: u8 = b'L';
const SDP_NAMESPACE: u8 = b'S';
const CHANNEL_NAMESPACE: u8 = b'C';

/// The namespace of the note set `index` in the store of its [`NoteSets`]:
/// `L` for the ledger set, `S` for the SDP set, and `C` followed by the
/// channel id for a channel set. No namespace is a prefix of another.
#[must_use]
pub fn note_set_namespace(index: NoteSetIndex) -> Bytes {
    match index {
        NoteSetIndex::Ledger => Bytes::from(vec![LEDGER_NAMESPACE]),
        NoteSetIndex::Sdp => Bytes::from(vec![SDP_NAMESPACE]),
        NoteSetIndex::Channel(channel_id) => {
            let mut namespace = Vec::with_capacity(33);
            namespace.push(CHANNEL_NAMESPACE);
            namespace.extend_from_slice(channel_id.as_ref());
            namespace.into()
        }
    }
}

fn in_memory_store() -> DynKvStore {
    ErasedKv::into_dyn(MemKv::new())
}

fn set_store(store: &DynKvStore, index: NoteSetIndex) -> NoteSetStore {
    Prefixed::new(Arc::clone(store), note_set_namespace(index))
}

/// The sets of a commit, the writes of their blocks, and the sets written to.
pub struct CommittedSets {
    pub sets: NoteSets,
    pub writes: WriteBatch,
    /// The sets with a block written, whose namespaces the writes touch.
    pub written: Vec<NoteSetIndex>,
}

impl NoteSets {
    /// The ledger and SDP sets, kept in a store of their own in memory.
    #[must_use]
    pub fn new() -> Self {
        Self::with_store(in_memory_store())
    }

    /// The ledger and SDP sets, kept in `store`.
    #[must_use]
    pub fn with_store(store: DynKvStore) -> Self {
        let ledger = NoteSet::empty_in(set_store(&store, NoteSetIndex::Ledger));
        let sdp = NoteSet::empty_in(set_store(&store, NoteSetIndex::Sdp));
        let (eligible, ledger_leaf) = EligibleTree::new().insert(ledger.eligible_leaf());
        let (eligible, sdp_leaf) = eligible.insert(sdp.eligible_leaf());
        debug_assert_eq!((ledger_leaf, sdp_leaf), (LEDGER_LEAF, SDP_LEAF));
        Self {
            store,
            ledger: TypedNoteSet {
                kind: Ledger,
                set: ledger,
            },
            sdp: TypedNoteSet {
                kind: Sdp,
                set: sdp,
            },
            channels: HashTrieMapSync::new_sync(),
            eligible,
            changed: HashTrieSetSync::new_sync(),
        }
    }

    #[must_use]
    pub const fn ledger(&self) -> &LedgerNoteSet {
        &self.ledger
    }

    #[must_use]
    pub const fn sdp(&self) -> &SdpNoteSet {
        &self.sdp
    }

    /// The note set of the channel, if it holds one.
    #[must_use]
    pub fn channel(&self, channel_id: &ChannelId) -> Option<&ChannelNoteSet> {
        self.channels.get(channel_id)
    }

    #[must_use]
    pub fn get(&self, index: NoteSetIndex) -> Option<&NoteSet> {
        match index {
            NoteSetIndex::Ledger => Some(&self.ledger),
            NoteSetIndex::Sdp => Some(&self.sdp),
            NoteSetIndex::Channel(channel_id) => self.channel(&channel_id).map(Deref::deref),
        }
    }

    #[must_use]
    pub fn update_ledger(self, set: LedgerNoteSet) -> Self {
        self.update_typed(set)
    }

    #[must_use]
    pub fn update_sdp(self, set: SdpNoteSet) -> Self {
        self.update_typed(set)
    }

    /// Replaces the note set of the channel of `set`.
    ///
    /// # Panics
    ///
    /// If the channel holds no note set.
    #[must_use]
    pub fn update_channel(self, set: ChannelNoteSet) -> Self {
        self.update_typed(set)
    }

    fn update_typed<K: SetKind>(self, set: TypedNoteSet<K>) -> Self {
        let index = set.index();
        self.update(index, set.into_inner())
    }

    /// The sets with an empty note set for the channel, at the next leaf of the
    /// eligible tree, unless the channel already holds one.
    #[must_use]
    pub fn with_channel(mut self, channel_id: ChannelId) -> Self {
        if self.channels.contains_key(&channel_id) {
            return self;
        }
        let set = NoteSet::empty_in(set_store(&self.store, NoteSetIndex::Channel(channel_id)));
        let (eligible, leaf) = self.eligible.insert(set.eligible_leaf());
        assert_eq!(
            leaf,
            FIRST_CHANNEL_LEAF + self.channels.size(),
            "eligible leaves follow the sets"
        );
        self.eligible = eligible;
        let kind = Channel {
            id: channel_id,
            leaf,
        };
        self.channels
            .insert_mut(channel_id, TypedNoteSet { kind, set });
        self
    }

    /// Replaces the set `index`, recording that its notes moved when they did.
    ///
    /// # Panics
    ///
    /// If `index` is a channel holding no note set.
    #[must_use]
    pub fn update(mut self, index: NoteSetIndex, set: NoteSet) -> Self {
        // Gas prices the end of block work only for a set whose notes moved.
        if !set.block_nullifiers.is_empty() || !set.block_commitments.is_empty() {
            self.changed.insert_mut(index);
        }
        self.replace(index, set);
        self
    }

    fn replace(&mut self, index: NoteSetIndex, set: NoteSet) {
        match index {
            NoteSetIndex::Ledger => self.ledger.set = set,
            NoteSetIndex::Sdp => self.sdp.set = set,
            NoteSetIndex::Channel(channel_id) => {
                let kind = self
                    .channel(&channel_id)
                    .unwrap_or_else(|| panic!("channel {channel_id} holds a note set"))
                    .kind;
                self.channels
                    .insert_mut(channel_id, TypedNoteSet { kind, set });
            }
        }
    }

    fn leaf(&self, index: NoteSetIndex) -> Option<usize> {
        match index {
            NoteSetIndex::Ledger => Some(LEDGER_LEAF),
            NoteSetIndex::Sdp => Some(SDP_LEAF),
            NoteSetIndex::Channel(channel_id) => {
                self.channel(&channel_id).map(|channel| channel.kind.leaf)
            }
        }
    }

    fn expect_set(&self, index: NoteSetIndex) -> &NoteSet {
        self.get(index)
            .unwrap_or_else(|| panic!("note set {index} exists"))
    }

    /// Spends `inputs` in the set `from` and adds `outputs` to the set `to` and
    /// to its buffer in `tx`, the two sets updated together.
    ///
    /// # Panics
    ///
    /// If `from` or `to` is not a note set.
    pub fn execute_transfer(
        self,
        tx: &mut TxNoteState,
        from: NoteSetIndex,
        inputs: &[NoteNullifier],
        to: NoteSetIndex,
        outputs: &[NoteCommitment],
    ) -> Result<Self, NoteSetError> {
        let spent = self.expect_set(from).clone().execute_spending(inputs)?;
        let sets = self.update(from, spent);
        sets.add_tx_outputs(tx, to, outputs)
    }

    /// Adds `outputs` to the set `index` and to its buffer in `tx`.
    ///
    /// # Panics
    ///
    /// If `index` is not a note set.
    pub fn add_tx_outputs(
        self,
        tx: &mut TxNoteState,
        index: NoteSetIndex,
        outputs: &[NoteCommitment],
    ) -> Result<Self, NoteSetError> {
        let added = self.expect_set(index).clone().execute_adding(outputs)?;
        tx.buffer_outputs(index, outputs);
        Ok(self.update(index, added))
    }

    pub fn end_block(mut self, block: BlockNumber) -> Result<Self, NoteSetError> {
        for index in self.changed.clone().iter().copied() {
            // `record_commitment_root` computes the commitment root of the set once.
            let set = self
                .expect_set(index)
                .clone()
                .record_commitment_root(block)?;
            let leaf = self.leaf(index).expect("a changed set exists");
            self.eligible = self.eligible.update(
                leaf,
                eligible_leaf(&set.committed_root, &set.sealed_nullifier_root()),
            );
            self.replace(index, set);
        }
        self.changed = HashTrieSetSync::new_sync();
        Ok(self)
    }

    /// As [`Self::end_block`], also returning what each changed set inserted
    /// and appended during the block, in the order of their eligible leaves.
    pub fn end_block_with_updates(
        self,
        block: BlockNumber,
    ) -> Result<(Self, Vec<NoteSetUpdate>), NoteSetError> {
        let mut changed: Vec<(usize, NoteSetIndex)> = self
            .changed
            .iter()
            .filter_map(|index| Some((self.leaf(*index)?, *index)))
            .collect();
        changed.sort_unstable_by_key(|(leaf, _)| *leaf);
        let updates = changed
            .into_iter()
            .map(|(_, index)| {
                let set = self.expect_set(index);
                NoteSetUpdate {
                    note_set: index,
                    nullifiers: set.block_nullifiers.iter().copied().collect(),
                    commitments: set.block_commitments.iter().copied().collect(),
                }
            })
            .collect();
        Ok((self.end_block(block)?, updates))
    }

    #[must_use]
    pub fn eligible_root(&self) -> Fr {
        self.eligible.root()
    }

    #[must_use]
    pub fn eligible_path(&self, index: NoteSetIndex) -> Option<MerklePath<Fr, 40>> {
        self.eligible.path(self.leaf(index)?)
    }

    // The channel sets in the order of their eligible leaves.
    fn channels_by_leaf(&self) -> Vec<(&ChannelId, &ChannelNoteSet)> {
        let mut channels: Vec<_> = self.channels.iter().collect();
        channels.sort_unstable_by_key(|(_, channel)| channel.kind.leaf);
        channels
    }

    // Every set with its index: the ledger set, the SDP set, then the channel
    // sets in no particular order.
    fn sets(&self) -> impl Iterator<Item = (NoteSetIndex, &NoteSet)> + '_ {
        [
            (NoteSetIndex::Ledger, &self.ledger.set),
            (NoteSetIndex::Sdp, &self.sdp.set),
        ]
        .into_iter()
        .chain(
            self.channels
                .iter()
                .map(|(channel_id, channel)| (NoteSetIndex::Channel(*channel_id), &channel.set)),
        )
    }

    /// The store the nullifier trees of the sets are kept in.
    #[must_use]
    pub const fn store(&self) -> &DynKvStore {
        &self.store
    }

    /// Whether a set has a block not committed yet.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.sets().any(|(_, set)| set.nullifiers.has_pending())
    }

    /// The length of the nullifier tree of each set.
    pub fn nullifier_lens(&self) -> impl Iterator<Item = (NoteSetIndex, u64)> + '_ {
        self.sets()
            .map(|(index, set)| (index, set.nullifiers.len()))
    }

    // Applies `step` to the nullifier tree of every set, gathering the writes
    // it returns as writes of the store of the sets, and the sets they touch.
    fn map_nullifiers(
        &self,
        mut step: impl FnMut(&NullifierImt) -> Result<(NullifierImt, WriteBatch), ImtError>,
    ) -> Result<CommittedSets, NoteSetError> {
        let mut sets = self.clone();
        let mut writes = WriteBatch::new();
        let mut written = Vec::new();
        for (index, set) in self.sets() {
            let (nullifiers, set_writes) = step(&set.nullifiers)?;
            if !set_writes.is_empty() {
                writes.append(set.nullifiers.store().namespace_batch(set_writes));
                written.push(index);
            }
            sets.replace(
                index,
                NoteSet {
                    nullifiers,
                    ..set.clone()
                },
            );
        }
        Ok(CommittedSets {
            sets,
            writes,
            written,
        })
    }

    /// Writes the block the sets are at the end of under `version`. The
    /// returned sets must not be used before the returned writes are applied
    /// to [`Self::store`]. A set with no pending block writes nothing.
    pub fn commit(&self, version: Version) -> Result<CommittedSets, NoteSetError> {
        self.map_nullifiers(|nullifiers| {
            let Committed { imt, writes } = nullifiers.commit(version)?;
            Ok((imt, writes))
        })
    }

    /// Makes the sets, those of block `block_id`, the finalized base of their
    /// store (see [`Imt::finalize`]).
    pub fn finalize(&self, block_id: BlockId) -> Result<(Self, WriteBatch), NoteSetError> {
        let finalized = self.map_nullifiers(|nullifiers| {
            let Finalized { imt, writes } = nullifiers.finalize(block_id)?;
            Ok((imt, writes))
        })?;
        Ok((finalized.sets, finalized.writes))
    }

    /// Drops from memory what the finalized base of the store holds (see
    /// [`Imt::rebase`]).
    pub fn rebase(&self) -> Result<Self, NoteSetError> {
        Ok(self
            .map_nullifiers(|nullifiers| Ok((nullifiers.rebase()?, WriteBatch::new())))?
            .sets)
    }

    /// The writes deleting what the block of `version` wrote in the sets
    /// `written` of `store`, once its fork is pruned.
    #[must_use]
    pub fn discard_writes(
        store: &DynKvStore,
        written: impl IntoIterator<Item = NoteSetIndex>,
        version: Version,
    ) -> WriteBatch {
        let mut batch = WriteBatch::new();
        for index in written {
            batch.append(
                set_store(store, index).namespace_batch(NullifierImt::discard_writes(version)),
            );
        }
        batch
    }

    /// The writes deleting the nodes of the set `index` of `store` that no
    /// state at least `floor_len` long reads (see [`Imt::collect_garbage`]).
    pub fn collect_garbage(
        store: &DynKvStore,
        index: NoteSetIndex,
        floor_len: u64,
    ) -> Result<WriteBatch, NoteSetError> {
        let set_store = set_store(store, index);
        let writes = NullifierImt::collect_garbage(&set_store, floor_len)?;
        Ok(set_store.namespace_batch(writes))
    }
}

// The encoding of a set. The nullifier tree is encoded by its leaves, as the
// tree kept in memory was, and a decoded set is imported in a store kept in
// memory.
#[derive(Serialize)]
#[serde(rename = "NoteSet")]
struct EncodedNoteSet<'a> {
    commitments: &'a CommitmentMmr,
    nullifiers: EncodedNullifiers,
    #[serde(with = "serde_fr")]
    committed_root: Fr,
    #[serde(with = "serde_recent_commitment_roots")]
    recent_commitment_roots: &'a RecentRoots,
}

#[derive(Serialize)]
#[serde(rename = "IndexedMerkleTree")]
struct EncodedNullifiers {
    leaves: Vec<NullifierLeaf>,
}

#[derive(Deserialize)]
#[serde(rename = "NoteSet")]
struct DecodedNoteSet {
    commitments: CommitmentMmr,
    nullifiers: CheckedNullifierImt,
    #[serde(with = "serde_fr")]
    committed_root: Fr,
    #[serde(with = "serde_recent_commitment_roots")]
    recent_commitment_roots: RecentRoots,
}

impl DecodedNoteSet {
    fn import(self, store: NoteSetStore) -> Result<NoteSet, ImtError> {
        let leaves: Vec<NullifierLeaf> = (0..self.nullifiers.len() as u64)
            .filter_map(|index| self.nullifiers.leaf(index).copied())
            .collect();
        let nullifiers = NullifierImt::import(store, &leaves, BlockId([0; 32]))?;
        Ok(NoteSet {
            committed_root: self.committed_root,
            recent_commitment_roots: self.recent_commitment_roots,
            ..NoteSet::with_nullifiers(self.commitments, nullifiers)
        })
    }
}

impl Serialize for NoteSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        EncodedNoteSet {
            commitments: &self.commitments,
            nullifiers: EncodedNullifiers {
                leaves: self.nullifiers.leaves().map_err(S::Error::custom)?,
            },
            committed_root: self.committed_root,
            recent_commitment_roots: &self.recent_commitment_roots,
        }
        .serialize(serializer)
    }
}

/// A set decoded alone is kept in a store of its own in memory.
impl<'de> Deserialize<'de> for NoteSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DecodedNoteSet::deserialize(deserializer)?
            .import(Prefixed::new(in_memory_store(), Bytes::new()))
            .map_err(D::Error::custom)
    }
}

// Only the sets are serialized, the channel sets in the order of their
// eligible leaves, because between blocks every eligible leaf is the leaf of
// its set.
#[derive(Serialize)]
struct EncodedNoteSetsRef<'a> {
    ledger: &'a NoteSet,
    sdp: &'a NoteSet,
    channels: Vec<(&'a ChannelId, &'a NoteSet)>,
}

#[derive(Deserialize)]
struct EncodedNoteSets {
    ledger: DecodedNoteSet,
    sdp: DecodedNoteSet,
    channels: Vec<(ChannelId, DecodedNoteSet)>,
}

impl Serialize for NoteSets {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        EncodedNoteSetsRef {
            ledger: &self.ledger.set,
            sdp: &self.sdp.set,
            channels: self
                .channels_by_leaf()
                .into_iter()
                .map(|(channel_id, channel)| (channel_id, &channel.set))
                .collect(),
        }
        .serialize(serializer)
    }
}

/// The decoded sets are kept in a store of their own in memory.
impl<'de> Deserialize<'de> for NoteSets {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let EncodedNoteSets {
            ledger,
            sdp,
            channels: decoded,
        } = EncodedNoteSets::deserialize(deserializer)?;
        let store = in_memory_store();
        let import = |index: NoteSetIndex, set: DecodedNoteSet| {
            set.import(set_store(&store, index))
                .map_err(D::Error::custom)
        };
        let ledger = import(NoteSetIndex::Ledger, ledger)?;
        let sdp = import(NoteSetIndex::Sdp, sdp)?;
        let mut channels = HashTrieMapSync::new_sync();
        for (position, (channel_id, set)) in decoded.into_iter().enumerate() {
            if channels.contains_key(&channel_id) {
                return Err(D::Error::custom(format!(
                    "channel {channel_id} holds two note sets"
                )));
            }
            let set = import(NoteSetIndex::Channel(channel_id), set)?;
            let kind = Channel {
                id: channel_id,
                leaf: FIRST_CHANNEL_LEAF + position,
            };
            channels.insert_mut(channel_id, TypedNoteSet { kind, set });
        }
        let mut leaves: Vec<(usize, Fr)> = vec![
            (LEDGER_LEAF, ledger.eligible_leaf()),
            (SDP_LEAF, sdp.eligible_leaf()),
        ];
        leaves.extend(
            channels
                .values()
                .map(|channel| (channel.kind.leaf, channel.eligible_leaf())),
        );
        leaves.sort_unstable_by_key(|(leaf, _)| *leaf);
        Ok(Self {
            store,
            ledger: TypedNoteSet {
                kind: Ledger,
                set: ledger,
            },
            sdp: TypedNoteSet {
                kind: Sdp,
                set: sdp,
            },
            channels,
            eligible: EligibleTree::from_sorted_items(leaves),
            changed: HashTrieSetSync::new_sync(),
        })
    }
}

#[must_use]
pub fn eligible_leaf(commitment_root: &Fr, nullifier_root: &Fr) -> Fr {
    ZkHasher::digest(&[*ELIGIBLE_SET_LEAF_V1, *commitment_root, *nullifier_root])
}

mod serde_recent_commitment_roots {
    use lb_groth16::{Fr, serde::serde_fr};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{BlockNumber, CommitmentMmr, RecentRoots};

    #[derive(Serialize, Deserialize)]
    struct Entry {
        #[serde(with = "serde_fr")]
        root: Fr,
        mmr: CommitmentMmr,
        superseded_at: BlockNumber,
    }

    pub fn serialize<S: Serializer>(roots: &RecentRoots, serializer: S) -> Result<S::Ok, S::Error> {
        // sorted by root so the encoding does not depend on the hash map order
        let mut entries: Vec<Entry> = roots
            .by_root
            .iter()
            .map(|(root, (mmr, superseded_at))| Entry {
                root: *root,
                mmr: mmr.clone(),
                superseded_at: *superseded_at,
            })
            .collect();
        entries.sort_by_key(|entry| entry.root);
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<RecentRoots, D::Error> {
        // the order by block is rebuilt from the entries
        let mut roots = RecentRoots::default();
        for entry in Vec::<Entry>::deserialize(deserializer)? {
            roots.insert_mut(entry.root, entry.mmr, entry.superseded_at);
        }
        Ok(roots)
    }
}

#[cfg(test)]
mod test {
    use ark_ff::AdditiveGroup as _;
    use lb_dynamic_merkle::MerkleNode;

    use super::*;

    fn commitment(n: u64) -> NoteCommitment {
        NoteCommitment(Fr::from(n))
    }

    fn nullifier(n: u64) -> NoteNullifier {
        NoteNullifier(Fr::from(n))
    }

    fn channel_id(n: u8) -> ChannelId {
        ChannelId::from([n; 32])
    }

    fn with_channel(sets: NoteSets, n: u8) -> (NoteSets, NoteSetIndex) {
        (
            sets.with_channel(channel_id(n)),
            NoteSetIndex::Channel(channel_id(n)),
        )
    }

    fn add_and_end_block(
        sets: NoteSets,
        note_commitment: NoteCommitment,
        block: BlockNumber,
    ) -> NoteSets {
        let set = sets
            .ledger()
            .clone()
            .execute_adding(&[note_commitment])
            .unwrap();
        sets.update_ledger(set).end_block(block).unwrap()
    }

    // The recent roots of `set` with the block at whose end each was superseded,
    // after checking the order by block holds the same entries.
    fn recent_entries(set: &NoteSet) -> HashMap<Fr, BlockNumber> {
        let roots = &set.recent_commitment_roots;
        let entries: HashMap<Fr, BlockNumber> = roots
            .by_root
            .iter()
            .map(|(root, (_, superseded_at))| (*root, *superseded_at))
            .collect();
        let ordered: HashMap<Fr, BlockNumber> = roots
            .by_superseded_at
            .iter()
            .map(|(superseded_at, root)| (*root, *superseded_at))
            .collect();
        assert_eq!(roots.by_superseded_at.size(), entries.len());
        assert_eq!(ordered, entries);
        entries
    }

    // The recent roots kept by scanning every one of them at the end of a block
    // that changed the set.
    struct ScannedRoots {
        committed: Fr,
        roots: HashMap<Fr, BlockNumber>,
    }

    impl ScannedRoots {
        fn record(&mut self, committed: Fr, block: BlockNumber) {
            if self.roots.contains_key(&self.committed) {
                self.roots.insert(self.committed, block);
            }
            self.committed = committed;
            self.roots.insert(committed, block);
            self.roots.retain(|root, superseded_at| {
                *root == committed || is_recent(*superseded_at, block)
            });
        }

        fn accepts(&self, root: &Fr, block: BlockNumber) -> bool {
            self.roots.get(root).is_some_and(|superseded_at| {
                *root == self.committed || is_recent(*superseded_at, block)
            })
        }
    }

    #[test]
    fn test_recent_roots_match_scanning_every_root() {
        let mut sets = NoteSets::new();
        let seed = sets.ledger().commitment_root();
        let mut scanned = ScannedRoots {
            committed: seed,
            roots: HashMap::from([(seed, 0)]),
        };
        let mut seen = vec![seed];
        for block in 0..3500 {
            // blocks that leave the set unchanged, among them a gap longer than the
            // window
            if block % 7 == 3 || (1500..2700).contains(&block) {
                continue;
            }
            let set = sets.ledger().clone();
            let set = if block % 5 == 1 {
                // a root current at the end of two blocks
                set.execute_spending(&[nullifier(block + 1)]).unwrap()
            } else if block % 11 == 4 {
                set.execute_adding(&[commitment(0)]).unwrap()
            } else {
                set.execute_adding(&[commitment(block + 1)]).unwrap()
            };
            sets = sets.update_ledger(set).end_block(block).unwrap();
            let set = sets.ledger();
            scanned.record(set.commitment_root(), block);
            if !seen.contains(&set.commitment_root()) {
                seen.push(set.commitment_root());
            }

            assert_eq!(recent_entries(set), scanned.roots, "block {block}");
            if block % 500 == 0 {
                let decoded: NoteSet =
                    bincode::deserialize(&bincode::serialize::<NoteSet>(set).unwrap()).unwrap();
                assert_eq!(recent_entries(&decoded), scanned.roots);
            }
            for next in [block + 1, block + 1024, block + 1025] {
                for root in &seen {
                    assert_eq!(
                        set.recent_mmr(root, next).is_ok(),
                        scanned.accepts(root, next),
                        "root {root} at block {next}"
                    );
                }
            }
        }
    }

    // Depth 40 root of the given leaves, every later position holding 0.
    fn naive_root_40(leaves: &[Fr]) -> Fr {
        let mut nodes = leaves.to_vec();
        let mut empty = Fr::ZERO;
        for _ in 0..40 {
            if nodes.len() % 2 == 1 {
                nodes.push(empty);
            }
            nodes = nodes
                .chunks(2)
                .map(|pair| ZkHasher::compress(&[pair[0], pair[1]]))
                .collect();
            empty = ZkHasher::compress(&[empty, empty]);
        }
        nodes.first().copied().unwrap_or(empty)
    }

    #[test]
    fn test_tx_commitment_root_appends_buffer_to_referenced_root() {
        let set = NoteSet::empty();
        let referenced = set.commitment_root();
        let buffer = TxBuffer::new().with(&[commitment(1), commitment(2)]);
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(2))
            .unwrap()
            .frontier_root();
        assert_eq!(
            set.tx_commitment_root(&buffer, &referenced, 1).unwrap(),
            expected
        );
        assert_eq!(
            set.step_commitment_root(&buffer, &referenced, 0, 1)
                .unwrap(),
            referenced
        );
        assert_eq!(
            set.step_commitment_root(&buffer, &referenced, 3, 1),
            Err(NoteSetError::BufferedTooLarge {
                buffered: 3,
                len: 2
            })
        );
    }

    #[test]
    fn only_an_update_moving_notes_gets_the_end_of_block_work_gas_prices() {
        let (sets, index) = with_channel(NoteSets::new(), 1);
        let set = sets.get(index).unwrap().clone();

        let empty = set.clone().execute_adding(&[]).unwrap();
        assert!(!sets.clone().update(index, empty).changed.contains(&index));

        let moved = set.execute_adding(&[commitment(1)]).unwrap();
        assert!(sets.update(index, moved).changed.contains(&index));
    }

    #[test]
    fn the_tx_state_buffers_only_the_set_added_to_and_caches_the_extended_root() {
        let (sets, channel) = with_channel(NoteSets::new(), 1);
        let referenced = sets.ledger().commitment_root();
        let mut tx = TxNoteState::new();
        let sets = sets
            .add_tx_outputs(&mut tx, NoteSetIndex::Ledger, &[commitment(1)])
            .unwrap();
        assert_eq!(tx.buffer(NoteSetIndex::Ledger).len(), 1);
        assert!(tx.buffer(channel).is_empty());
        // Nothing added, nothing buffered
        tx.buffer_outputs(channel, &[]);
        assert!(!tx.buffers.contains_key(&channel));

        let set = sets.ledger();
        let root = tx.commitment_root(set, &referenced, 1).unwrap();
        assert_eq!(
            Ok(root),
            set.tx_commitment_root(&tx.buffer(NoteSetIndex::Ledger), &referenced, 1)
        );
        assert!(
            tx.roots()
                .0
                .borrow()
                .values()
                .any(|extended| extended.root == Some(root))
        );
    }

    #[test]
    fn test_tx_roots_match_step_commitment_root_in_any_order() {
        let set = NoteSet::empty();
        let referenced = set.commitment_root();
        let tx_roots = TxRoots::default();
        let buffer = TxBuffer::new().with(&[commitment(1), commitment(2), commitment(3)]);
        for buffered in [2, 0, 3, 1] {
            assert_eq!(
                set.cached_step_commitment_root(&tx_roots, &buffer, &referenced, buffered, 1),
                set.step_commitment_root(&buffer, &referenced, buffered, 1)
            );
        }
        let buffer = buffer.with(&[commitment(4)]);
        assert_eq!(
            set.cached_step_commitment_root(&tx_roots, &buffer, &referenced, 4, 1),
            set.step_commitment_root(&buffer, &referenced, 4, 1)
        );
        assert_eq!(
            set.cached_step_commitment_root(&tx_roots, &buffer, &referenced, 5, 1),
            Err(NoteSetError::BufferedTooLarge {
                buffered: 5,
                len: 4
            })
        );
        assert_eq!(
            set.cached_step_commitment_root(&tx_roots, &buffer, &Fr::from(7u64), 0, 1),
            Err(NoteSetError::UnknownRoot(Fr::from(7u64)))
        );
    }

    #[test]
    fn test_nothing_buffered_gives_the_referenced_root() {
        let seed = NoteSet::empty().commitment_root();
        let sets = add_and_end_block(NoteSets::new(), commitment(1), 1);
        let set = sets.ledger().clone();
        let current = set.commitment_root();
        let buffer = TxBuffer::new().with(&[commitment(2), commitment(3)]);
        let empty = TxBuffer::new();
        let tx_roots = TxRoots::default();
        for referenced in [seed, current] {
            let snapshot = set.recent_mmr(&referenced, 1024).unwrap().frontier_root();
            assert_eq!(snapshot, referenced);
            assert_eq!(
                set.step_commitment_root(&buffer, &referenced, 0, 1024),
                Ok(referenced)
            );
            assert_eq!(
                set.cached_step_commitment_root(&tx_roots, &buffer, &referenced, 0, 1024),
                Ok(referenced)
            );
            assert_eq!(
                set.tx_commitment_root(&empty, &referenced, 1024),
                Ok(referenced)
            );
            assert_eq!(
                set.cached_step_commitment_root(&tx_roots, &empty, &referenced, 0, 1024),
                Ok(referenced)
            );
        }
        // the seed root was superseded at block 1
        let unknown = Fr::from(7u64);
        for (referenced, block) in [(seed, 1025), (unknown, 1)] {
            let rejected = Err(NoteSetError::UnknownRoot(referenced));
            assert_eq!(
                set.step_commitment_root(&buffer, &referenced, 0, block),
                rejected
            );
            assert_eq!(
                set.cached_step_commitment_root(&tx_roots, &buffer, &referenced, 0, block),
                rejected
            );
            assert_eq!(set.tx_commitment_root(&empty, &referenced, block), rejected);
        }
        assert_eq!(
            set.step_commitment_root(&buffer, &current, 0, 5000),
            Ok(current)
        );
    }

    #[test]
    fn test_tx_roots_resume_from_checkpoints_and_ignore_other_buffers() {
        let set = NoteSet::empty();
        let referenced = set.commitment_root();
        let tx_roots = TxRoots::default();
        let outputs: Vec<NoteCommitment> = (1..=200).map(commitment).collect();
        let buffer = TxBuffer::new().with(&outputs);
        for buffered in [200, 130, 64, 65, 199, 1] {
            assert_eq!(
                set.cached_step_commitment_root(&tx_roots, &buffer, &referenced, buffered, 1),
                set.step_commitment_root(&buffer, &referenced, buffered, 1)
            );
        }
        // Another buffer shares the cache but none of its prefixes
        let other = TxBuffer::new().with(&[commitment(1), commitment(9)]);
        assert_eq!(
            set.cached_step_commitment_root(&tx_roots, &other, &referenced, 2, 1),
            set.step_commitment_root(&other, &referenced, 2, 1)
        );
    }

    #[test]
    fn test_root_window_edges() {
        let sets = NoteSets::new();
        let old = sets.ledger().commitment_root();
        let sets = add_and_end_block(sets, commitment(1), 1); // `old` last current at block 0
        let set = sets.ledger();
        assert!(set.tx_commitment_root(&TxBuffer::new(), &old, 1024).is_ok());
        assert_eq!(
            set.tx_commitment_root(&TxBuffer::new(), &old, 1025),
            Err(NoteSetError::UnknownRoot(old))
        );
        assert!(
            set.tx_commitment_root(&TxBuffer::new(), &set.commitment_root(), 5000)
                .is_ok()
        ); // untouched set
    }

    #[test]
    fn test_seed_root_leaves_the_window_1024_blocks_after_the_set_first_changes() {
        let empty = NoteSet::empty().commitment_root();

        // Genesis changes the ledger set at block 0: the seed root was current before
        // it.
        let sets = add_and_end_block(NoteSets::new(), commitment(1), 0);
        let set = sets.ledger();
        assert!(
            set.tx_commitment_root(&TxBuffer::new(), &empty, 1023)
                .is_ok()
        );
        assert_eq!(
            set.tx_commitment_root(&TxBuffer::new(), &empty, 1024),
            Err(NoteSetError::UnknownRoot(empty))
        );

        // A channel set first changed at block 10.
        let (sets, channel) = with_channel(sets, 1);
        let channel_set = sets
            .get(channel)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(2)])
            .unwrap();
        let sets = sets.update(channel, channel_set).end_block(10).unwrap();
        let set = sets.get(channel).unwrap();
        assert!(
            set.tx_commitment_root(&TxBuffer::new(), &empty, 1033)
                .is_ok()
        );
        assert_eq!(
            set.tx_commitment_root(&TxBuffer::new(), &empty, 1034),
            Err(NoteSetError::UnknownRoot(empty))
        );
    }

    #[test]
    fn test_execute_transfer_spends_in_one_set_and_adds_and_buffers_in_another() {
        let (sets, channel) = with_channel(NoteSets::new(), 1);
        let mut tx = TxNoteState::new();
        let sets = sets
            .execute_transfer(
                &mut tx,
                NoteSetIndex::Ledger,
                &[nullifier(1)],
                channel,
                &[commitment(2)],
            )
            .unwrap();
        let ledger = sets.ledger();
        let channel_set = sets.get(channel).unwrap();
        assert!(ledger.contains_nullifier(&nullifier(1)).unwrap());
        assert!(tx.buffer(NoteSetIndex::Ledger).is_empty());
        assert_ne!(
            channel_set.commitment_root(),
            NoteSet::empty().commitment_root()
        );
        assert_eq!(tx.buffer(channel).len(), 1);

        // Within one set, the spend and the outputs land in the same set
        let sets = sets
            .execute_transfer(
                &mut tx,
                NoteSetIndex::Ledger,
                &[nullifier(3)],
                NoteSetIndex::Ledger,
                &[commitment(4)],
            )
            .unwrap();
        let ledger = sets.ledger();
        assert!(ledger.contains_nullifier(&nullifier(1)).unwrap());
        assert!(ledger.contains_nullifier(&nullifier(3)).unwrap());
        assert_eq!(tx.buffer(NoteSetIndex::Ledger).len(), 1);

        // A spent input fails the whole transfer
        assert_eq!(
            sets.execute_transfer(
                &mut tx,
                NoteSetIndex::Ledger,
                &[nullifier(3)],
                channel,
                &[commitment(5)]
            ),
            Err(NoteSetError::Imt(ImtError::AlreadyPresent(nullifier(3).0)))
        );
    }

    #[test]
    fn test_spendable_rejects_empty_duplicate_spent() {
        let set = NoteSet::empty();
        assert_eq!(set.assert_spendable(&[]), Err(NoteSetError::EmptyInputs));
        assert_eq!(
            set.assert_spendable(&[nullifier(1), nullifier(2), nullifier(1)]),
            Err(NoteSetError::DuplicateInput(nullifier(1)))
        );
        let set = set.execute_spending(&[nullifier(2)]).unwrap();
        assert!(set.contains_nullifier(&nullifier(2)).unwrap());
        assert_eq!(
            set.assert_spendable(&[nullifier(1), nullifier(2)]),
            Err(NoteSetError::AlreadySpent(nullifier(2)))
        );
        assert!(set.assert_spendable(&[nullifier(1)]).is_ok());
    }

    #[test]
    fn test_a_new_transaction_starts_with_empty_buffers() {
        let sets = NoteSets::new();
        let mut tx = TxNoteState::new();
        let sets = sets
            .add_tx_outputs(&mut tx, NoteSetIndex::Ledger, &[commitment(1)])
            .unwrap();
        let set = sets.ledger();
        let referenced = NoteSet::empty().commitment_root();
        assert_ne!(tx.commitment_root(set, &referenced, 0), Ok(referenced));
        let tx = TxNoteState::new();
        assert_eq!(tx.commitment_root(set, &referenced, 0), Ok(referenced));
    }

    #[test]
    fn test_eligible_root_matches_naive_over_sets_in_creation_order() {
        // The second channel created sorts before the first by id, the leaves
        // follow the creation order.
        let (sets, second) = with_channel(NoteSets::new(), 9);
        let (sets, first) = with_channel(sets, 1);
        let channel_set = sets
            .get(first)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(2)])
            .unwrap();
        let sets = add_and_end_block(sets.update(first, channel_set), commitment(1), 1);
        let leaves: Vec<Fr> = [NoteSetIndex::Ledger, NoteSetIndex::Sdp, second, first]
            .into_iter()
            .map(|index| sets.get(index).unwrap().eligible_leaf())
            .collect();
        assert_eq!(sets.eligible_root(), naive_root_40(&leaves));
        assert_eq!(sets.leaf(second), Some(2));
        assert_eq!(sets.leaf(first), Some(3));
        assert_eq!(sets.leaf(NoteSetIndex::Channel(channel_id(5))), None);
    }

    #[test]
    fn note_set_index_keys_a_json_map_and_round_trips_in_binary() {
        let indexes = [
            NoteSetIndex::Ledger,
            NoteSetIndex::Sdp,
            NoteSetIndex::Channel(channel_id(0xab)),
        ];
        let map: HashMap<NoteSetIndex, u8> = indexes.iter().copied().zip(0..).collect();
        let json = serde_json::to_string(&map).unwrap();
        assert!(json.contains(&format!("\"channel:{}\"", "ab".repeat(32))));
        assert_eq!(
            serde_json::from_str::<HashMap<NoteSetIndex, u8>>(&json).unwrap(),
            map
        );
        for index in indexes {
            let bytes = bincode::serialize(&index).unwrap();
            assert_eq!(bincode::deserialize::<NoteSetIndex>(&bytes).unwrap(), index);
            assert_eq!(index.to_string().parse::<NoteSetIndex>(), Ok(index));
        }
        for bad in ["", "channel", "channel:zz", "channel:abab", "Ledger"] {
            assert!(bad.parse::<NoteSetIndex>().is_err(), "{bad}");
        }
    }

    #[test]
    fn with_channel_keeps_an_existing_channel_set() {
        let (sets, channel) = with_channel(NoteSets::new(), 1);
        let channel_set = sets
            .get(channel)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(2)])
            .unwrap();
        let sets = sets.update(channel, channel_set);
        assert_eq!(sets.clone().with_channel(channel_id(1)), sets);
    }

    #[test]
    fn a_typed_set_keeps_its_set_through_spending_and_adding() {
        let (sets, _) = with_channel(NoteSets::new(), 1);
        let (sets, channel) = with_channel(sets, 2);
        assert_eq!(sets.ledger().index(), NoteSetIndex::Ledger);
        assert_eq!(sets.sdp().index(), NoteSetIndex::Sdp);

        let channel_set = sets
            .channel(&channel_id(2))
            .unwrap()
            .clone()
            .execute_adding(&[commitment(1)])
            .unwrap()
            .execute_spending(&[nullifier(1)])
            .unwrap();
        assert_eq!(channel_set.index(), channel);

        let sets = sets.update_channel(channel_set.clone());
        assert_eq!(sets.channel(&channel_id(2)), Some(&channel_set));
        assert_eq!(sets.leaf(channel), Some(FIRST_CHANNEL_LEAF + 1));
        assert_eq!(**sets.channel(&channel_id(1)).unwrap(), NoteSet::empty());
    }

    #[test]
    fn test_serde_rebuilds_the_eligible_tree() {
        let (sets, channel) = with_channel(NoteSets::new(), 1);
        let old = sets.ledger().commitment_root();
        let sets = add_and_end_block(sets, commitment(1), 1);
        let channel_set = sets
            .get(channel)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(2)])
            .unwrap()
            .execute_spending(&[nullifier(3)])
            .unwrap();
        let sets = sets.update(channel, channel_set).end_block(2).unwrap();

        let decoded: NoteSets =
            serde_json::from_str(&serde_json::to_string(&sets).unwrap()).unwrap();
        assert_eq!(decoded, sets);
        assert_eq!(decoded.eligible_root(), sets.eligible_root());
        // `MerkleNode` has no `PartialEq`, so the paths compare as (side, sibling)
        let nodes = |path: MerklePath<Fr, 40>| {
            path.map(|node| (matches!(node, MerkleNode::Left(_)), *node.item()))
        };
        assert_eq!(
            decoded.eligible_path(channel).map(nodes),
            sets.eligible_path(channel).map(nodes)
        );
        assert!(
            decoded
                .ledger()
                .tx_commitment_root(&TxBuffer::new(), &old, 1024)
                .is_ok()
        );

        let bytes = bincode::serialize(&sets).unwrap();
        assert_eq!(bincode::deserialize::<NoteSets>(&bytes).unwrap(), sets);
    }

    #[test]
    fn test_deserialize_rejects_fewer_than_two_sets() {
        let one = serde_json::to_string(&vec![NoteSet::empty()]).unwrap();
        assert!(serde_json::from_str::<NoteSets>(&one).is_err());
        assert!(serde_json::from_str::<NoteSets>("[]").is_err());
    }

    #[test]
    fn test_end_block_reports_and_clears_the_block_log() {
        let (sets, channel) = with_channel(NoteSets::new(), 1);
        let ledger = sets
            .ledger()
            .clone()
            .execute_adding(&[commitment(1), commitment(2)])
            .unwrap()
            .execute_spending(&[nullifier(7)])
            .unwrap();
        let channel_set = sets
            .get(channel)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(3)])
            .unwrap();
        let sets = sets.update(channel, channel_set).update_ledger(ledger);

        let (sets, updates) = sets.end_block_with_updates(1).unwrap();
        assert_eq!(
            updates,
            vec![
                NoteSetUpdate {
                    note_set: NoteSetIndex::Ledger,
                    nullifiers: vec![nullifier(7)],
                    commitments: vec![commitment(1), commitment(2)]
                },
                NoteSetUpdate {
                    note_set: channel,
                    nullifiers: vec![],
                    commitments: vec![commitment(3)]
                },
            ]
        );
        let ledger = sets
            .ledger()
            .clone()
            .execute_adding(&[commitment(4)])
            .unwrap();
        let (_, next) = sets
            .update_ledger(ledger)
            .end_block_with_updates(2)
            .unwrap();
        assert_eq!(
            next,
            vec![NoteSetUpdate {
                note_set: NoteSetIndex::Ledger,
                nullifiers: vec![],
                commitments: vec![commitment(4)]
            }]
        );
    }

    #[test]
    fn test_accessors_expose_the_set_trees() {
        let set = NoteSet::empty()
            .execute_adding(&[commitment(1)])
            .unwrap()
            .execute_spending(&[nullifier(2)])
            .unwrap();
        assert_eq!(set.commitments().frontier_root(), set.commitment_root());
        assert_eq!(
            set.nullifiers().root().unwrap(),
            set.nullifier_root().unwrap()
        );
        assert!(
            set.nullifiers()
                .low_leaf(&nullifier(2).0)
                .unwrap()
                .is_none()
        );
    }
}
