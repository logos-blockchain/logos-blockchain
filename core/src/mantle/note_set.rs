use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, LazyLock, Mutex, PoisonError},
};

use lb_dynamic_merkle::{DynamicMerkleTree, MerklePath};
use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes, serde::serde_fr};
use lb_imt::{ImtError, IndexedMerkleTree};
use lb_mmr::MerkleMountainRange;
use lb_poseidon2::Digest as _;
use lb_utxotree::UtxoMerkleHasher;
use rpds::{HashTrieMapSync, HashTrieSetSync, VectorSync};
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

// The extended MMRs computed during the current Mantle Transaction, shared by
// its Operations; not state, so emptied with every transaction and block.
#[derive(Clone, Default)]
pub struct TxRoots(Arc<Mutex<HashMap<TxRootKey, CommitmentMmr>>>);

impl fmt::Debug for TxRoots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TxRoots")
    }
}

impl PartialEq for TxRoots {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for TxRoots {}

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
    recent_commitment_roots: HashTrieMapSync<Fr, (CommitmentMmr, BlockNumber)>,
    // commitments added by the previous Operations of the Mantle Transaction
    #[serde(skip)]
    tx_commitment_buffer: VectorSync<NoteCommitment>,
    // the id of each prefix of `tx_commitment_buffer`, ending at each commitment
    #[serde(skip)]
    tx_commitment_prefix_ids: VectorSync<PrefixId>,
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
            recent_commitment_roots: HashTrieMapSync::new_sync()
                .insert(committed_root, (commitments.clone(), 0)),
            commitments,
            nullifiers: NullifierImt::new(),
            committed_root,
            tx_commitment_buffer: VectorSync::new_sync(),
            tx_commitment_prefix_ids: VectorSync::new_sync(),
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

    pub fn assert_spendable(
        &self,
        inputs: &[NoteNullifier],
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<(), NoteSetError> {
        if inputs.is_empty() {
            return Err(NoteSetError::EmptyInputs);
        }
        let mut unique = HashSet::with_capacity(inputs.len());
        for input in inputs {
            if !unique.insert(input) {
                return Err(NoteSetError::DuplicateInput(*input));
            }
        }
        self.recent_mmr(commitment_merkle_root, block)?;
        for input in inputs {
            if self.contains_nullifier(input) {
                return Err(NoteSetError::AlreadySpent(*input));
            }
        }
        Ok(())
    }

    pub fn tx_commitment_root(
        &self,
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        self.step_commitment_root(
            commitment_merkle_root,
            self.tx_commitment_buffer.len(),
            block,
        )
    }

    pub fn step_commitment_root(
        &self,
        commitment_merkle_root: &Fr,
        buffered: usize,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        if buffered > self.tx_commitment_buffer.len() {
            return Err(NoteSetError::BufferedTooLarge {
                buffered,
                len: self.tx_commitment_buffer.len(),
            });
        }
        let mut mmr = self.recent_mmr(commitment_merkle_root, block)?.clone();
        for note_commitment in self.tx_commitment_buffer.iter().take(buffered) {
            mmr = mmr
                .push(*note_commitment)
                .map_err(|_| NoteSetError::MmrFull)?;
        }
        Ok(mmr.frontier_root())
    }

    pub fn cached_tx_commitment_root(
        &self,
        tx_roots: &TxRoots,
        commitment_merkle_root: &Fr,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        self.cached_step_commitment_root(
            tx_roots,
            commitment_merkle_root,
            self.tx_commitment_buffer.len(),
            block,
        )
    }

    // Same root as `step_commitment_root`, resuming from the closest extended MMR
    // `tx_roots` holds for the referenced root instead of replaying the whole
    // buffer for every Operation and step.
    pub fn cached_step_commitment_root(
        &self,
        tx_roots: &TxRoots,
        commitment_merkle_root: &Fr,
        buffered: usize,
        block: BlockNumber,
    ) -> Result<Fr, NoteSetError> {
        if buffered > self.tx_commitment_buffer.len() {
            return Err(NoteSetError::BufferedTooLarge {
                buffered,
                len: self.tx_commitment_buffer.len(),
            });
        }
        let base = self.recent_mmr(commitment_merkle_root, block)?;
        let key = |len: usize| {
            let prefix_id = len
                .checked_sub(1)
                .map_or(EMPTY_PREFIX, |last| self.tx_commitment_prefix_ids[last]);
            (*commitment_merkle_root, base.len(), prefix_id)
        };
        let mut cache = tx_roots.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut start = buffered;
        let mut mmr = loop {
            if let Some(mmr) = cache.get(&key(start)) {
                break mmr.clone();
            }
            if start == 0 {
                break base.clone();
            }
            start = (start - 1) / PREFIX_CHECKPOINT * PREFIX_CHECKPOINT;
        };
        for len in start + 1..=buffered {
            mmr = mmr
                .push(self.tx_commitment_buffer[len - 1])
                .map_err(|_| NoteSetError::MmrFull)?;
            if len % PREFIX_CHECKPOINT == 0 {
                cache.insert(key(len), mmr.clone());
            }
        }
        cache.insert(key(buffered), mmr.clone());
        drop(cache);
        Ok(mmr.frontier_root())
    }

    #[must_use]
    pub fn buffer_outputs(mut self, outputs: &[NoteCommitment]) -> Self {
        for note_commitment in outputs {
            let previous = self
                .tx_commitment_prefix_ids
                .last()
                .copied()
                .unwrap_or(EMPTY_PREFIX);
            let prefix_id: PrefixId = Hasher::new()
                .chain_update(previous)
                .chain_update(fr_to_bytes(&note_commitment.0))
                .finalize()
                .into();
            self.tx_commitment_buffer.push_back_mut(*note_commitment);
            self.tx_commitment_prefix_ids.push_back_mut(prefix_id);
        }
        self
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
        match self.recent_commitment_roots.get(commitment_merkle_root) {
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
        if let Some((mmr, _)) = self.recent_commitment_roots.get(&previous).cloned() {
            // The empty seed root is current before block 0, so a root is
            // stored with the block that superseded it.
            self.recent_commitment_roots
                .insert_mut(previous, (mmr, block));
        }
        self.committed_root = self.commitment_root();
        self.recent_commitment_roots
            .insert_mut(self.committed_root, (self.commitments.clone(), block));
        let expired: Vec<Fr> = self
            .recent_commitment_roots
            .iter()
            .filter(|(root, (_, superseded_at))| {
                **root != self.committed_root && !is_recent(*superseded_at, block)
            })
            .map(|(root, _)| *root)
            .collect();
        for root in &expired {
            self.recent_commitment_roots.remove_mut(root);
        }
        self.tx_commitment_buffer = VectorSync::new_sync();
        self.tx_commitment_prefix_ids = VectorSync::new_sync();
        self.block_nullifiers = VectorSync::new_sync();
        self.block_commitments = VectorSync::new_sync();
        self
    }
}

const fn is_recent(superseded_at: BlockNumber, block: BlockNumber) -> bool {
    superseded_at.saturating_add(RECENT_ROOTS_WINDOW) > block
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteSets {
    sets: VectorSync<NoteSet>,
    eligible: EligibleTree,
    // sets touched in the current block
    changed: HashTrieSetSync<NoteSetIndex>,
    tx_roots: TxRoots,
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
            tx_roots: TxRoots::default(),
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
        assert!(self.sets.set_mut(position, set), "note set {index} exists");
        self.changed.insert_mut(index);
        self
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
    pub fn start_transaction(mut self) -> Self {
        for index in &self.changed {
            let position = usize::try_from(*index).expect("note set index fits in usize");
            let mut set = self.sets[position].clone();
            set.tx_commitment_buffer = VectorSync::new_sync();
            set.tx_commitment_prefix_ids = VectorSync::new_sync();
            self.sets.set_mut(position, set);
        }
        self.tx_roots = TxRoots::default();
        self
    }

    #[must_use]
    pub const fn tx_roots(&self) -> &TxRoots {
        &self.tx_roots
    }

    #[must_use]
    pub fn end_block(mut self, block: BlockNumber) -> Self {
        for index in &self.changed {
            let position = usize::try_from(*index).expect("note set index fits in usize");
            let set = self.sets[position].clone().record_commitment_root(block);
            self.eligible = self.eligible.update(
                position,
                eligible_leaf(&set.commitment_root(), &set.nullifier_root()),
            );
            self.sets.set_mut(position, set);
        }
        self.changed = HashTrieSetSync::new_sync();
        self.tx_roots = TxRoots::default();
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
                if set.block_nullifiers.is_empty() && set.block_commitments.is_empty() {
                    return None;
                }
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
            tx_roots: TxRoots::default(),
        })
    }
}

#[must_use]
pub fn eligible_leaf(commitment_root: &Fr, nullifier_root: &Fr) -> Fr {
    ZkHasher::digest(&[*ELIGIBLE_SET_LEAF_V1, *commitment_root, *nullifier_root])
}

mod serde_recent_commitment_roots {
    use lb_groth16::{Fr, serde::serde_fr};
    use rpds::HashTrieMapSync;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{BlockNumber, CommitmentMmr};

    #[derive(Serialize, Deserialize)]
    struct Entry {
        #[serde(with = "serde_fr")]
        root: Fr,
        mmr: CommitmentMmr,
        superseded_at: BlockNumber,
    }

    pub fn serialize<S: Serializer>(
        roots: &HashTrieMapSync<Fr, (CommitmentMmr, BlockNumber)>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // sorted by root so the encoding does not depend on the hash map order
        let mut entries: Vec<Entry> = roots
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
    ) -> Result<HashTrieMapSync<Fr, (CommitmentMmr, BlockNumber)>, D::Error> {
        let mut roots = HashTrieMapSync::new_sync();
        for entry in Vec::<Entry>::deserialize(deserializer)? {
            roots.insert_mut(entry.root, (entry.mmr, entry.superseded_at));
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
        let set = set.buffer_outputs(&[commitment(1), commitment(2)]);
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(2))
            .unwrap()
            .frontier_root();
        assert_eq!(set.tx_commitment_root(&referenced, 1).unwrap(), expected);
        assert_eq!(
            set.step_commitment_root(&referenced, 0, 1).unwrap(),
            referenced
        );
        assert_eq!(
            set.step_commitment_root(&referenced, 3, 1),
            Err(NoteSetError::BufferedTooLarge {
                buffered: 3,
                len: 2
            })
        );
    }

    #[test]
    fn test_tx_roots_match_step_commitment_root_in_any_order() {
        let set = NoteSet::empty();
        let referenced = set.commitment_root();
        let tx_roots = TxRoots::default();
        let set = set.buffer_outputs(&[commitment(1), commitment(2), commitment(3)]);
        for buffered in [2, 0, 3, 1] {
            assert_eq!(
                set.cached_step_commitment_root(&tx_roots, &referenced, buffered, 1),
                set.step_commitment_root(&referenced, buffered, 1)
            );
        }
        let set = set.buffer_outputs(&[commitment(4)]);
        assert_eq!(
            set.cached_step_commitment_root(&tx_roots, &referenced, 4, 1),
            set.step_commitment_root(&referenced, 4, 1)
        );
        assert_eq!(
            set.cached_step_commitment_root(&tx_roots, &referenced, 5, 1),
            Err(NoteSetError::BufferedTooLarge {
                buffered: 5,
                len: 4
            })
        );
        assert_eq!(
            set.cached_step_commitment_root(&tx_roots, &Fr::from(7u64), 0, 1),
            Err(NoteSetError::UnknownRoot(Fr::from(7u64)))
        );
    }

    #[test]
    fn test_tx_roots_resume_from_checkpoints_and_ignore_other_buffers() {
        let set = NoteSet::empty();
        let referenced = set.commitment_root();
        let tx_roots = TxRoots::default();
        let outputs: Vec<NoteCommitment> = (1..=200).map(commitment).collect();
        let set = set.buffer_outputs(&outputs);
        for buffered in [200, 130, 64, 65, 199, 1] {
            assert_eq!(
                set.cached_step_commitment_root(&tx_roots, &referenced, buffered, 1),
                set.step_commitment_root(&referenced, buffered, 1)
            );
        }
        // A clone with another buffer shares the cache but none of its prefixes
        let other = NoteSet::empty().buffer_outputs(&[commitment(1), commitment(9)]);
        assert_eq!(
            other.cached_step_commitment_root(&tx_roots, &referenced, 2, 1),
            other.step_commitment_root(&referenced, 2, 1)
        );
    }

    #[test]
    fn test_root_window_edges() {
        let sets = NoteSets::new();
        let old = sets.get(LEDGER_SET).unwrap().commitment_root();
        let sets = add_and_end_block(sets, commitment(1), 1); // `old` last current at block 0
        let set = sets.get(LEDGER_SET).unwrap();
        assert!(set.assert_spendable(&[nullifier(9)], &old, 1024).is_ok());
        assert_eq!(
            set.assert_spendable(&[nullifier(9)], &old, 1025),
            Err(NoteSetError::UnknownRoot(old))
        );
        assert!(
            set.assert_spendable(&[nullifier(9)], &set.commitment_root(), 5000)
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
        assert!(set.assert_spendable(&[nullifier(9)], &empty, 1023).is_ok());
        assert_eq!(
            set.assert_spendable(&[nullifier(9)], &empty, 1024),
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
        assert!(set.assert_spendable(&[nullifier(9)], &empty, 1033).is_ok());
        assert_eq!(
            set.assert_spendable(&[nullifier(9)], &empty, 1034),
            Err(NoteSetError::UnknownRoot(empty))
        );
    }

    #[test]
    fn test_spendable_rejects_empty_duplicate_spent() {
        let set = NoteSet::empty();
        let root = set.commitment_root();
        assert_eq!(
            set.assert_spendable(&[], &root, 0),
            Err(NoteSetError::EmptyInputs)
        );
        assert_eq!(
            set.assert_spendable(&[nullifier(1), nullifier(2), nullifier(1)], &root, 0),
            Err(NoteSetError::DuplicateInput(nullifier(1)))
        );
        let set = set.execute_spending(&[nullifier(2)]).unwrap();
        assert!(set.contains_nullifier(&nullifier(2)));
        assert_eq!(
            set.assert_spendable(&[nullifier(1), nullifier(2)], &root, 0),
            Err(NoteSetError::AlreadySpent(nullifier(2)))
        );
        assert!(set.assert_spendable(&[nullifier(1)], &root, 0).is_ok());
    }

    #[test]
    fn test_buffer_cleared_between_transactions() {
        let sets = NoteSets::new();
        let set = sets
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(1)]);
        let sets = sets.update(LEDGER_SET, set).start_transaction();
        let set = sets.get(LEDGER_SET).unwrap();
        assert_eq!(
            set.tx_commitment_root(&set.commitment_root(), 0).unwrap(),
            set.commitment_root()
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
                .assert_spendable(&[nullifier(9)], &old, 1024)
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
