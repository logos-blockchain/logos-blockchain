use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt,
    sync::LazyLock,
};

use lb_dynamic_merkle::{DynamicMerkleTree, MerklePath};
use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes, serde::serde_fr};
use lb_imt::{ImtError, IndexedMerkleTree};
use lb_mmr::MerkleMountainRange;
use lb_poseidon2::Digest as _;
use lb_utxotree::UtxoMerkleHasher;
use rpds::{HashTrieMapSync, HashTrieSetSync, RedBlackTreeSetSync, VectorSync};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{
    block::BlockNumber,
    crypto::{Digest as _, Hasher, ZkHasher},
    mantle::notes::{NoteCommitment, NoteNullifier},
};

pub type NoteSetIndex = u64;

pub const LEDGER_SET: NoteSetIndex = 0;
pub const SDP_SET: NoteSetIndex = 1;

// A root can be referenced while validating the 1024 blocks after the last
// block it was current at.
pub const RECENT_ROOTS_WINDOW: BlockNumber = 1024;

pub type CommitmentMmr = MerkleMountainRange<NoteCommitment, ZkHasher, 41>;
pub type NullifierImt = IndexedMerkleTree<ZkHasher, 40>;
type EligibleTree = DynamicMerkleTree<UtxoMerkleHasher<ZkHasher>, 40>;

static ELIGIBLE_SET_LEAF_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"ELIGIBLE_SET_LEAF_V1").expect("BigUint should load from constant string")
});

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
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

static EMPTY_BUFFER: TxBuffer = TxBuffer::new();

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
    pub fn buffer(&self, index: NoteSetIndex) -> &TxBuffer {
        self.buffers.get(&index).unwrap_or(&EMPTY_BUFFER)
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

    /// Checks that `inputs` are spendable in `set`, the set `index`, and
    /// returns the root a `ZkTransfer` spending them is proven against:
    /// `commitment_merkle_root` extended with the buffer of the set.
    pub fn spendable_root(
        &self,
        index: NoteSetIndex,
        set: &NoteSet,
        inputs: &[NoteNullifier],
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        set.assert_spendable(inputs)?;
        self.commitment_root(index, set, commitment_merkle_root, block)
    }

    /// `commitment_merkle_root` of `set`, the set `index`, extended with its
    /// buffer.
    pub fn commitment_root(
        &self,
        index: NoteSetIndex,
        set: &NoteSet,
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        let buffer = self.buffer(index);
        set.cached_step_commitment_root(
            &self.roots,
            buffer,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSet {
    commitments: CommitmentMmr,
    nullifiers: NullifierImt,
    // root at the end of the last block that changed the set
    #[serde(with = "serde_fr")]
    committed_root: Fr,
    // snapshot of each recent root, with the block at whose end it was superseded
    #[serde(with = "serde_recent_commitment_roots")]
    recent_commitment_roots: RecentRoots,
    // what the set inserted and appended during the current block, reported at its end
    #[serde(skip)]
    block_nullifiers: VectorSync<NoteNullifier>,
    #[serde(skip)]
    block_commitments: VectorSync<NoteCommitment>,
}

impl NoteSet {
    #[must_use]
    pub fn empty() -> Self {
        let commitments = CommitmentMmr::new();
        let committed_root = commitments.frontier_root();
        Self {
            recent_commitment_roots: {
                let mut roots = RecentRoots::default();
                roots.insert_mut(committed_root, commitments.clone(), 0);
                roots
            },
            commitments,
            nullifiers: NullifierImt::new(),
            committed_root,
            block_nullifiers: VectorSync::new_sync(),
            block_commitments: VectorSync::new_sync(),
        }
    }

    #[must_use]
    pub fn commitment_root(&self) -> Fr {
        self.commitments.frontier_root()
    }

    #[must_use]
    pub fn nullifier_root(&self) -> Fr {
        self.nullifiers.root()
    }

    #[must_use]
    pub const fn commitments(&self) -> &CommitmentMmr {
        &self.commitments
    }

    #[must_use]
    pub const fn nullifiers(&self) -> &NullifierImt {
        &self.nullifiers
    }

    #[must_use]
    pub fn contains_nullifier(&self, nullifier: &NoteNullifier) -> bool {
        self.nullifiers.contains(&nullifier.0)
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
            if self.contains_nullifier(input) {
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
    fn record_commitment_root(mut self, block: BlockNumber) -> Self {
        self.nullifiers = self.nullifiers.flush();
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
        self
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteSets {
    sets: VectorSync<NoteSet>,
    eligible: EligibleTree,
    // sets whose notes moved in the current block
    changed: HashTrieSetSync<NoteSetIndex>,
}

impl Default for NoteSets {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteSets {
    #[must_use]
    pub fn new() -> Self {
        let sets = Self {
            sets: VectorSync::new_sync(),
            eligible: EligibleTree::new(),
            changed: HashTrieSetSync::new_sync(),
        };
        let (sets, ledger) = sets.push_empty();
        let (sets, sdp) = sets.push_empty();
        debug_assert_eq!((ledger, sdp), (LEDGER_SET, SDP_SET));
        sets
    }

    #[must_use]
    pub fn get(&self, index: NoteSetIndex) -> Option<&NoteSet> {
        self.sets.get(usize::try_from(index).ok()?)
    }

    #[must_use]
    pub fn update(mut self, index: NoteSetIndex, set: NoteSet) -> Self {
        let position = usize::try_from(index).expect("note set index fits in usize");
        // Gas prices the end of block work only for a set whose notes moved.
        if !set.block_nullifiers.is_empty() || !set.block_commitments.is_empty() {
            self.changed.insert_mut(index);
        }
        assert!(self.sets.set_mut(position, set), "note set {index} exists");
        self
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
        let spent = self
            .get(from)
            .unwrap_or_else(|| panic!("note set {from} exists"))
            .clone()
            .execute_spending(inputs)?;
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
        let added = self
            .get(index)
            .unwrap_or_else(|| panic!("note set {index} exists"))
            .clone()
            .execute_adding(outputs)?;
        tx.buffer_outputs(index, outputs);
        Ok(self.update(index, added))
    }

    #[must_use]
    pub fn push_empty(mut self) -> (Self, NoteSetIndex) {
        let set = NoteSet::empty();
        let (eligible, position) = self
            .eligible
            .insert(eligible_leaf(&set.commitment_root(), &set.nullifier_root()));
        assert_eq!(position, self.sets.len(), "eligible leaves follow the sets");
        self.eligible = eligible;
        self.sets.push_back_mut(set);
        (self, position as NoteSetIndex)
    }

    #[must_use]
    pub fn end_block(mut self, block: BlockNumber) -> Self {
        for index in &self.changed {
            let position = usize::try_from(*index).expect("note set index fits in usize");
            // `record_commitment_root` computes the commitment root of the set once.
            let set = self.sets[position].clone().record_commitment_root(block);
            self.eligible = self.eligible.update(
                position,
                eligible_leaf(&set.committed_root, &set.nullifier_root()),
            );
            self.sets.set_mut(position, set);
        }
        self.changed = HashTrieSetSync::new_sync();
        self
    }

    #[must_use]
    pub fn end_block_with_updates(self, block: BlockNumber) -> (Self, Vec<NoteSetUpdate>) {
        let mut changed: Vec<NoteSetIndex> = self.changed.iter().copied().collect();
        changed.sort_unstable();
        let updates = changed
            .into_iter()
            .filter_map(|index| {
                let set = self.get(index)?;
                Some(NoteSetUpdate {
                    note_set: index,
                    nullifiers: set.block_nullifiers.iter().copied().collect(),
                    commitments: set.block_commitments.iter().copied().collect(),
                })
            })
            .collect();
        (self.end_block(block), updates)
    }

    #[must_use]
    pub fn eligible_root(&self) -> Fr {
        self.eligible.root()
    }

    #[must_use]
    pub fn eligible_path(&self, index: NoteSetIndex) -> Option<MerklePath<Fr, 40>> {
        self.eligible.path(usize::try_from(index).ok()?)
    }
}

// Only the sets are serialized because between blocks every eligible leaf is
// the leaf of its set.
impl Serialize for NoteSets {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.sets.iter().collect::<Vec<_>>().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NoteSets {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let decoded = Vec::<NoteSet>::deserialize(deserializer)?;
        if decoded.len() < 2 {
            return Err(D::Error::custom(
                "note sets must hold the ledger and SDP sets",
            ));
        }
        let mut sets = VectorSync::new_sync();
        for set in decoded {
            sets.push_back_mut(set);
        }
        let eligible =
            EligibleTree::from_sorted_items(sets.iter().enumerate().map(|(position, set)| {
                (
                    position,
                    eligible_leaf(&set.commitment_root(), &set.nullifier_root()),
                )
            }));
        Ok(Self {
            sets,
            eligible,
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

    fn add_and_end_block(
        sets: NoteSets,
        note_commitment: NoteCommitment,
        block: BlockNumber,
    ) -> NoteSets {
        let set = sets
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .execute_adding(&[note_commitment])
            .unwrap();
        sets.update(LEDGER_SET, set).end_block(block)
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
        let seed = sets.get(LEDGER_SET).unwrap().commitment_root();
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
            let set = sets.get(LEDGER_SET).unwrap().clone();
            let set = if block % 5 == 1 {
                // a root current at the end of two blocks
                set.execute_spending(&[nullifier(block + 1)]).unwrap()
            } else if block % 11 == 4 {
                set.execute_adding(&[commitment(0)]).unwrap()
            } else {
                set.execute_adding(&[commitment(block + 1)]).unwrap()
            };
            sets = sets.update(LEDGER_SET, set).end_block(block);
            let set = sets.get(LEDGER_SET).unwrap();
            scanned.record(set.commitment_root(), block);
            if !seen.contains(&set.commitment_root()) {
                seen.push(set.commitment_root());
            }

            assert_eq!(recent_entries(set), scanned.roots, "block {block}");
            if block % 500 == 0 {
                let decoded: NoteSet =
                    bincode::deserialize(&bincode::serialize(set).unwrap()).unwrap();
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
        let (sets, index) = NoteSets::new().push_empty();
        let set = sets.get(index).unwrap().clone();

        let empty = set.clone().execute_adding(&[]).unwrap();
        assert!(!sets.clone().update(index, empty).changed.contains(&index));

        let moved = set.execute_adding(&[commitment(1)]).unwrap();
        assert!(sets.update(index, moved).changed.contains(&index));
    }

    #[test]
    fn the_tx_state_buffers_only_the_set_added_to_and_caches_the_extended_root() {
        let (sets, channel) = NoteSets::new().push_empty();
        let referenced = sets.get(LEDGER_SET).unwrap().commitment_root();
        let mut tx = TxNoteState::new();
        let sets = sets
            .add_tx_outputs(&mut tx, LEDGER_SET, &[commitment(1)])
            .unwrap();
        assert_eq!(tx.buffer(LEDGER_SET).len(), 1);
        assert!(tx.buffer(channel).is_empty());
        // Nothing added, nothing buffered
        tx.buffer_outputs(channel, &[]);
        assert!(!tx.buffers.contains_key(&channel));

        let set = sets.get(LEDGER_SET).unwrap();
        let root = tx.commitment_root(LEDGER_SET, set, &referenced, 1).unwrap();
        assert_eq!(
            Ok(root),
            set.tx_commitment_root(tx.buffer(LEDGER_SET), &referenced, 1)
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
        let set = sets.get(LEDGER_SET).unwrap().clone();
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
        let old = sets.get(LEDGER_SET).unwrap().commitment_root();
        let sets = add_and_end_block(sets, commitment(1), 1); // `old` last current at block 0
        let set = sets.get(LEDGER_SET).unwrap();
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
        let set = sets.get(LEDGER_SET).unwrap();
        assert!(
            set.tx_commitment_root(&TxBuffer::new(), &empty, 1023)
                .is_ok()
        );
        assert_eq!(
            set.tx_commitment_root(&TxBuffer::new(), &empty, 1024),
            Err(NoteSetError::UnknownRoot(empty))
        );

        // A channel set first changed at block 10.
        let (sets, channel) = sets.push_empty();
        let channel_set = sets
            .get(channel)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(2)])
            .unwrap();
        let sets = sets.update(channel, channel_set).end_block(10);
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
        let (sets, channel) = NoteSets::new().push_empty();
        let mut tx = TxNoteState::new();
        let sets = sets
            .execute_transfer(
                &mut tx,
                LEDGER_SET,
                &[nullifier(1)],
                channel,
                &[commitment(2)],
            )
            .unwrap();
        let ledger = sets.get(LEDGER_SET).unwrap();
        let channel_set = sets.get(channel).unwrap();
        assert!(ledger.contains_nullifier(&nullifier(1)));
        assert!(tx.buffer(LEDGER_SET).is_empty());
        assert_ne!(
            channel_set.commitment_root(),
            NoteSet::empty().commitment_root()
        );
        assert_eq!(tx.buffer(channel).len(), 1);

        // Within one set, the spend and the outputs land in the same set
        let sets = sets
            .execute_transfer(
                &mut tx,
                LEDGER_SET,
                &[nullifier(3)],
                LEDGER_SET,
                &[commitment(4)],
            )
            .unwrap();
        let ledger = sets.get(LEDGER_SET).unwrap();
        assert!(ledger.contains_nullifier(&nullifier(1)));
        assert!(ledger.contains_nullifier(&nullifier(3)));
        assert_eq!(tx.buffer(LEDGER_SET).len(), 1);

        // A spent input fails the whole transfer
        assert_eq!(
            sets.execute_transfer(
                &mut tx,
                LEDGER_SET,
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
        assert!(set.contains_nullifier(&nullifier(2)));
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
            .add_tx_outputs(&mut tx, LEDGER_SET, &[commitment(1)])
            .unwrap();
        let set = sets.get(LEDGER_SET).unwrap();
        let referenced = NoteSet::empty().commitment_root();
        assert_ne!(
            tx.commitment_root(LEDGER_SET, set, &referenced, 0),
            Ok(referenced)
        );
        let tx = TxNoteState::new();
        assert_eq!(
            tx.commitment_root(LEDGER_SET, set, &referenced, 0),
            Ok(referenced)
        );
    }

    #[test]
    fn test_eligible_root_matches_naive_over_sets() {
        let (sets, channel) = NoteSets::new().push_empty();
        assert_eq!(channel, 2);
        let sets = add_and_end_block(sets, commitment(1), 1);
        let leaves: Vec<Fr> = (0..3)
            .map(|i| {
                let s = sets.get(i).unwrap();
                eligible_leaf(&s.commitment_root(), &s.nullifier_root())
            })
            .collect();
        assert_eq!(sets.eligible_root(), naive_root_40(&leaves));
    }

    #[test]
    fn test_serde_rebuilds_the_eligible_tree() {
        let (sets, channel) = NoteSets::new().push_empty();
        let old = sets.get(LEDGER_SET).unwrap().commitment_root();
        let sets = add_and_end_block(sets, commitment(1), 1);
        let channel_set = sets
            .get(channel)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(2)])
            .unwrap()
            .execute_spending(&[nullifier(3)])
            .unwrap();
        let sets = sets.update(channel, channel_set).end_block(2);

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
                .get(LEDGER_SET)
                .unwrap()
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
        let (sets, channel) = NoteSets::new().push_empty();
        let ledger = sets
            .get(LEDGER_SET)
            .unwrap()
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
        let sets = sets.update(channel, channel_set).update(LEDGER_SET, ledger);

        let (sets, updates) = sets.end_block_with_updates(1);
        assert_eq!(
            updates,
            vec![
                NoteSetUpdate {
                    note_set: LEDGER_SET,
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
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(4)])
            .unwrap();
        let (_, next) = sets.update(LEDGER_SET, ledger).end_block_with_updates(2);
        assert_eq!(
            next,
            vec![NoteSetUpdate {
                note_set: LEDGER_SET,
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
        assert_eq!(set.nullifiers().root(), set.nullifier_root());
        assert!(set.nullifiers().low_leaf(&nullifier(2).0).is_none());
    }
}
