//! The unfinalized L1 window as the channel sees it: every block above LIB
//! with its parent link, its cumulative safe set, its cumulative channel-note
//! set and its classified channel content, plus the finalized tips and note
//! set the branch reads fall back to.

use std::collections::{BTreeMap, HashMap, HashSet};

use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps,
        ledger::{NoteId, verification_mode::StandardMode},
        ops::channel::MsgId,
        transactions::{hash::TxHash, states::Unverified},
    },
};
use rpds::{HashTrieMapSync, HashTrieSetSync};

use super::types::{
    AtomicWithdrawInfo, ChannelNote, ChannelWalletView, InscriptionInfo, PinDepositInfo,
};

type NoteSet = HashTrieMapSync<NoteId, ChannelNote>;

/// One channel-note mutation, in on-chain execution order.
#[derive(Debug, Clone)]
pub(super) enum NoteOp {
    Add(ChannelNote),
    Remove(NoteId),
}

/// One block's cumulative state, built from its parent's plus the block's
/// own content.
#[derive(Debug, Clone, Default)]
struct BlockState {
    /// Tx hashes mined on the branch through this block.
    safe_set: HashTrieSetSync<TxHash>,
    /// Channel notes alive at this block.
    notes: NoteSet,
}

#[derive(Debug)]
pub struct BlockStates {
    /// Per-block cumulative safe sets and note sets.
    block_states: BTreeMap<HeaderId, BlockState>,
    /// Block parent relationships for pruning.
    parent_map: HashMap<HeaderId, HeaderId>,
    /// Current LIB for pruning. On a cold start this is the node's LIB until
    /// the replay of the history below it reaches a block, from then on the
    /// newest finalized block processed.
    current_lib: HeaderId,
    /// Per L1 block channel content (unfinalized window), canonical or not.
    block_txs: HashMap<HeaderId, StoredBlock>,
    /// Last finalized channel tip — used as parent when pending is empty.
    finalized_msg: MsgId,
    /// Lineage-parent of the entry behind [`Self::finalized_msg`] — the
    /// finalized entry is matched as a `(this_msg, parent_msg)` pair. `None`
    /// when the finalized entry is unknown (fresh state or checkpoint
    /// restore); the finalized-prefix search then matches nothing (see
    /// `TxState::finalized_prefix_ids`).
    finalized_parent_msg: Option<MsgId>,
    /// The config-lineage tip at LIB — the newest config finalized so far, or
    /// [`MsgId::root`] when none has finalized (or is unknown after a
    /// checkpoint restore). Seeds the landable set in
    /// `TxState::shed_stale_pending_configs` so a pending config chaining on
    /// the finalized config tip is not falsely orphaned.
    finalized_config: MsgId,
    /// The note set at the LIB block. Splits a branch's notes into finalized
    /// and unfinalized, seeds a parentless block and is exported for
    /// checkpointing.
    finalized_notes: NoteSet,
}

/// One stored L1 block's channel content.
#[derive(Debug, Default)]
struct StoredBlock {
    /// Channel-touching txs, classified at block scan.
    channel_txs: Vec<BlockChannelTx>,
    /// Mirrorable channel txs, the source for (re-)mirroring into pending
    /// whenever the block is on the canonical path.
    ///
    /// TODO(zone-sdk): a canonical block's bytes are duplicated here and in
    /// pending; share them (`Arc`) between the store and the pending entries
    /// so the store is the single owner. Tracked as a follow-up refactor.
    signed_txs: Vec<SignedOps<Unverified, StandardMode>>,
}

/// A channel-touching tx's tip-advancing content, classified once at block
/// scan and stored per block.
#[derive(Debug, Clone)]
pub enum BlockChannelTx {
    /// `publish` shape: a single inscription.
    Inscription(InscriptionInfo),
    /// `publish_atomic_withdraw` shape: an inscription + its withdraws.
    AtomicWithdraw(AtomicWithdrawInfo),
    /// `publish_pin_deposit` shape: an inscription + a transfer
    /// consuming an observed deposited note.
    PinDeposit(PinDepositInfo),
    /// A pure `channel_config` tx: a single config on the config lineage
    /// (`this_msg` = config id, `parent_msg` = config parent), which does not
    /// advance the message tip.
    Config(InscriptionInfo),
    /// A shape the SDK cannot produce (bundled deposits, multi-inscribe,
    /// custom-built txs). Kept whole — updates hand the tx back to the
    /// caller's own recovery logic — along with its inscriptions in op order
    /// (`message_entries`) and any `ChannelConfig` ops it carries
    /// (`config_entries`), which sit on the separate config lineage and never
    /// advance the message tip.
    Custom {
        tx: SignedOps<Unverified, StandardMode>,
        message_entries: Vec<InscriptionInfo>,
        config_entries: Vec<InscriptionInfo>,
    },
}

impl BlockChannelTx {
    /// The tip-advancing entries of this tx, in op order.
    pub fn infos(&self) -> &[InscriptionInfo] {
        match self {
            Self::Inscription(i) => std::slice::from_ref(i),
            Self::AtomicWithdraw(a) => std::slice::from_ref(&a.inscription),
            Self::PinDeposit(a) => std::slice::from_ref(&a.inscription),
            Self::Config(_) => &[],
            Self::Custom {
                message_entries, ..
            } => message_entries,
        }
    }

    /// The `ChannelConfig` entries this tx carries, in op order. These are on
    /// the config lineage (`this_msg` = config id, `parent_msg` = config
    /// parent) and do not advance the message tip. The clean
    /// `Inscription`/`AtomicWithdraw` shapes carry none; a pure config is a
    /// [`Self::Config`]; mixed/unknown configs ride in [`Self::Custom`].
    pub fn config_entries(&self) -> &[InscriptionInfo] {
        match self {
            Self::Inscription(_) | Self::AtomicWithdraw(_) | Self::PinDeposit(_) => &[],
            Self::Config(c) => std::slice::from_ref(c),
            Self::Custom { config_entries, .. } => config_entries,
        }
    }

    /// The entry this tx leaves the channel at (its last tip-advancing op).
    fn tip_entry(&self) -> Option<&InscriptionInfo> {
        self.infos().last()
    }

    #[must_use]
    pub fn tx_hash(&self) -> Option<TxHash> {
        self.infos()
            .first()
            .or_else(|| self.config_entries().first())
            .map(|i| i.tx_hash)
    }
}

impl BlockStates {
    #[must_use]
    pub fn new(lib: HeaderId, finalized_msg: MsgId) -> Self {
        let mut block_states = BTreeMap::new();
        block_states.insert(lib, BlockState::default());
        Self {
            block_states,
            parent_map: HashMap::new(),
            current_lib: lib,
            block_txs: HashMap::new(),
            finalized_msg,
            finalized_parent_msg: None,
            finalized_config: MsgId::root(),
            finalized_notes: NoteSet::new_sync(),
        }
    }

    /// Current LIB.
    #[must_use]
    pub const fn lib(&self) -> HeaderId {
        self.current_lib
    }

    /// Check if we have state for a block.
    #[must_use]
    pub fn has_block(&self, block_id: &HeaderId) -> bool {
        self.block_states.contains_key(block_id)
    }

    /// The finalized channel tip; see [`Self::set_finalized_msg`].
    #[must_use]
    pub const fn finalized_msg(&self) -> MsgId {
        self.finalized_msg
    }

    /// The lineage-parent of the finalized channel tip, `None` when unknown.
    #[must_use]
    pub const fn finalized_parent_msg(&self) -> Option<MsgId> {
        self.finalized_parent_msg
    }

    /// Update the finalized channel tip from backfilled finalized history.
    /// `parent` is the entry's lineage-parent; pass `None` only when it is
    /// genuinely unknown (disables the finalized-prefix mask until the next
    /// boundary move records a parent).
    pub const fn set_finalized_msg(&mut self, msg: MsgId, parent: Option<MsgId>) {
        self.finalized_msg = msg;
        self.finalized_parent_msg = parent;
    }

    /// The finalized config-lineage tip (the newest config at/below LIB). Read
    /// for checkpointing; restored via [`Self::set_finalized_config`].
    #[must_use]
    pub const fn finalized_config(&self) -> MsgId {
        self.finalized_config
    }

    /// Restore/refresh the finalized config-lineage tip — from a checkpoint on
    /// warm start, or from backfilled finalized history. Without this the tip
    /// resets to [`MsgId::root`] on restart, and `config_tip_at` can fall back
    /// to a stale parent once the config's block is pruned below LIB.
    pub const fn set_finalized_config(&mut self, config: MsgId) {
        self.finalized_config = config;
    }

    /// Record `block_id` under `parent_id` with its classified channel txs;
    /// `our_txs` are the hashes that become safe at this block and
    /// `note_ops` the channel-note changes it applies, in execution order.
    pub fn insert(
        &mut self,
        block_id: HeaderId,
        parent_id: HeaderId,
        our_txs: impl IntoIterator<Item = TxHash>,
        channel_txs: Vec<BlockChannelTx>,
        note_ops: Vec<NoteOp>,
    ) {
        // Store parent relationship for pruning
        self.parent_map.insert(block_id, parent_id);

        // Build cumulative state from the parent. Parent may be missing
        // when blocks are processed from slot-range backfill and LIB has
        // advanced between batches (pruning the parent). Starting the safe
        // set empty is conservative: txs show as "pending" until seen in a
        // subsequent block with a known parent. Notes start from the
        // finalized set, the most complete set known below the block.
        //
        // Every channel-tip-touching tx joins the set, tracked or not: a fork
        // block is not mirrored on arrival, and its txs must read as mined
        // once the branch turns canonical and the store re-mirrors them.
        let parent = self.block_states.get(&parent_id);
        let mut safe_set = parent.map(|p| p.safe_set.clone()).unwrap_or_default();
        let mut notes = parent.map_or_else(|| self.finalized_notes.clone(), |p| p.notes.clone());

        for tx in our_txs {
            safe_set = safe_set.insert(tx);
        }
        for op in note_ops {
            notes = match op {
                NoteOp::Add(note) => notes.insert(note.note_id, note),
                NoteOp::Remove(id) => notes.remove(&id),
            };
        }
        self.block_states
            .insert(block_id, BlockState { safe_set, notes });

        // Store the block's classified channel txs
        if !channel_txs.is_empty() {
            self.block_txs.entry(block_id).or_default().channel_txs = channel_txs;
        }
    }

    /// Move LIB to `lib`: advance the finalized tips and note set, prune the
    /// blocks at or below the old window and drop finalized hashes from the
    /// safe sets. `is_pending` marks hashes still tracked as pending, which
    /// stay in the sets.
    pub fn advance_lib(&mut self, lib: HeaderId, is_pending: impl Fn(&TxHash) -> bool) {
        // Compute finalized_msg BEFORE pruning — walk from new LIB
        // backwards to find the latest inscription in the finalized range.
        // Keep its lineage-parent too: the (id, parent) pair is what
        // identifies the finalized position in `finalized_prefix_ids`.
        if let Some((msg, parent)) = self
            .channel_tip_entry_at(lib)
            .map(|entry| (entry.this_msg, entry.parent_msg))
        {
            self.finalized_msg = msg;
            self.finalized_parent_msg = Some(parent);
        }
        // Advance the finalized config tip too (same pre-prune walk), so a
        // pending config chaining on it stays landable after LIB moves.
        if let Some(config) = self.config_tip_entry_at(lib) {
            self.finalized_config = config.this_msg;
        }
        if let Some(state) = self.block_states.get(&lib) {
            self.finalized_notes = state.notes.clone();
        }

        // Prune ancestors of new lib (but not lib itself)
        let mut prune_cursor = self.parent_map.get(&lib).copied();
        while let Some(b) = prune_cursor {
            self.block_states.remove(&b);
            self.block_txs.remove(&b);
            prune_cursor = self.parent_map.remove(&b);
        }

        // Remove finalized tx hashes from all safe sets. Using remove
        // (rather than rebuild) preserves rpds memory sharing between
        // block states for non-finalized txs.
        if let Some(lib_state) = self.block_states.get(&lib) {
            let finalized_hashes: Vec<TxHash> = lib_state
                .safe_set
                .iter()
                .filter(|hash| !is_pending(hash))
                .copied()
                .collect();
            for state in self.block_states.values_mut() {
                for tx_hash in &finalized_hashes {
                    state.safe_set = state.safe_set.remove(tx_hash);
                }
            }
        }

        self.prune_orphans(lib);
        self.current_lib = lib;
    }

    /// The channel's note set at `tip`, split into the notes also in the
    /// finalized set and the rest; the finalized set alone when `tip` is
    /// `None` or unknown.
    pub fn notes_at(&self, tip: Option<HeaderId>) -> ChannelWalletView {
        let notes = tip
            .and_then(|tip| self.block_states.get(&tip))
            .map_or(&self.finalized_notes, |state| &state.notes);
        let (finalized, unfinalized): (Vec<_>, Vec<_>) = notes
            .values()
            .cloned()
            .partition(|note| self.finalized_notes.contains_key(&note.note_id));
        ChannelWalletView {
            finalized,
            unfinalized,
        }
    }

    /// A tracked note by id, in the finalized set or at any stored block. A
    /// `NoteId` is a content commitment, so `id → (value, pk)` is a function
    /// and any tracked copy is authoritative regardless of branch — enough
    /// to recover a consumed note's value/key.
    pub fn find_note(&self, id: &NoteId) -> Option<&ChannelNote> {
        self.finalized_notes.get(id).or_else(|| {
            self.block_states
                .values()
                .find_map(|state| state.notes.get(id))
        })
    }

    /// The finalized note set, for checkpointing.
    pub fn finalized_notes(&self) -> Vec<ChannelNote> {
        self.finalized_notes.values().cloned().collect()
    }

    /// Restore the finalized note set from a checkpoint; the LIB block
    /// starts from it.
    pub fn restore_finalized_notes(&mut self, notes: Vec<ChannelNote>) {
        let notes: NoteSet = notes.into_iter().map(|n| (n.note_id, n)).collect();
        if let Some(state) = self.block_states.get_mut(&self.current_lib) {
            state.notes = notes.clone();
        }
        self.finalized_notes = notes;
    }

    /// Remove orphaned blocks whose parent was pruned.
    fn prune_orphans(&mut self, lib: HeaderId) {
        loop {
            let orphans: Vec<_> = self
                .parent_map
                .iter()
                .filter_map(|(id, parent)| {
                    if *id == lib {
                        return None; // lib is root
                    }
                    let parent_is_lib = *parent == lib;
                    let parent_exists = self.parent_map.contains_key(parent);
                    (!parent_is_lib && !parent_exists).then_some(*id)
                })
                .collect();

            if orphans.is_empty() {
                break;
            }

            for orphan in orphans {
                self.block_states.remove(&orphan);
                self.block_txs.remove(&orphan);
                self.parent_map.remove(&orphan);
            }
        }
    }

    /// Tx hashes mined on `tip`'s branch (its cumulative safe set).
    pub fn safe_at(&self, tip: HeaderId) -> HashSet<TxHash> {
        self.block_states
            .get(&tip)
            .map(|s| s.safe_set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Blocks on `tip`'s branch strictly above LIB, oldest first.
    pub fn branch_blocks_above_lib(&self, tip: HeaderId) -> Vec<HeaderId> {
        let mut blocks = Vec::new();
        let mut current = tip;
        while current != self.current_lib {
            blocks.push(current);
            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => break,
            }
        }
        blocks.reverse();
        blocks
    }

    /// Derive the channel tip `MsgId` at a given L1 block by walking backwards
    /// through the block tree and finding the most recent inscription.
    /// Returns `finalized_msg` if no inscriptions are found in the
    /// unfinalized window.
    #[must_use]
    pub fn channel_tip_at(&self, block_id: HeaderId) -> MsgId {
        self.channel_tip_entry_at(block_id)
            .map_or(self.finalized_msg, |entry| entry.this_msg)
    }

    /// Like [`Self::channel_tip_at`], but returns the tip-advancing entry
    /// itself so callers can also learn its lineage-parent. `None` when no
    /// entry exists in the walked window (the finalized boundary applies).
    fn channel_tip_entry_at(&self, block_id: HeaderId) -> Option<&InscriptionInfo> {
        let mut current = block_id;
        loop {
            if let Some(block) = self.block_txs.get(&current)
                && let Some(entry) = block
                    .channel_txs
                    .iter()
                    .rev()
                    .find_map(BlockChannelTx::tip_entry)
            {
                return Some(entry);
            }

            if current == self.current_lib {
                return None;
            }

            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => return None,
            }
        }
    }

    /// The config-lineage tip entry at a block: the most recent config in the
    /// walked window (block → LIB). Mirrors [`Self::channel_tip_entry_at`] but
    /// over the config lineage; `None` when no config exists in the window.
    fn config_tip_entry_at(&self, block_id: HeaderId) -> Option<&InscriptionInfo> {
        let mut current = block_id;
        loop {
            if let Some(block) = self.block_txs.get(&current)
                && let Some(entry) = block
                    .channel_txs
                    .iter()
                    .rev()
                    .find_map(|tx| tx.config_entries().last())
            {
                return Some(entry);
            }

            if current == self.current_lib {
                return None;
            }

            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => return None,
            }
        }
    }

    /// The config-lineage tip at a block: the most recent config in the walked
    /// window (block → LIB), or [`Self::finalized_config`] if none. Derived
    /// only from blocks we have processed, so — unlike the node's single config
    /// tip — it never races ahead of local state.
    #[must_use]
    pub fn config_tip_at(&self, block_id: HeaderId) -> MsgId {
        self.config_tip_entry_at(block_id)
            .map_or(self.finalized_config, |entry| entry.this_msg)
    }

    /// All tip-advancing entries on a branch back to LIB, oldest first:
    /// message entries and config entries, in op order.
    pub fn infos_on_branch(&self, tip: HeaderId) -> Vec<InscriptionInfo> {
        let mut blocks = Vec::new();
        let mut current = tip;

        loop {
            blocks.push(current);
            if current == self.current_lib {
                break;
            }
            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => break,
            }
        }

        blocks.reverse();
        blocks
            .into_iter()
            .flat_map(|block_id| {
                self.block_txs
                    .get(&block_id)
                    .map_or_else(Vec::new, |block| {
                        block
                            .channel_txs
                            .iter()
                            .flat_map(|tx| tx.infos().iter().chain(tx.config_entries()))
                            .cloned()
                            .collect()
                    })
            })
            .collect()
    }

    /// Keep a block's mirrorable signed channel txs.
    pub fn store_signed_txs(
        &mut self,
        block: HeaderId,
        txs: Vec<SignedOps<Unverified, StandardMode>>,
    ) {
        if !txs.is_empty() {
            self.block_txs.entry(block).or_default().signed_txs = txs;
        }
    }

    /// Signed channel txs on `tip`'s branch strictly above LIB, oldest block
    /// first.
    pub fn signed_txs_on_branch(
        &self,
        tip: HeaderId,
    ) -> impl Iterator<Item = &SignedOps<Unverified, StandardMode>> {
        self.branch_blocks_above_lib(tip)
            .into_iter()
            .filter_map(|id| self.block_txs.get(&id))
            .flat_map(|block| block.signed_txs.iter())
    }

    /// A mined channel tx by hash, with the signed txs of its block.
    pub fn channel_tx(
        &self,
        tx_hash: TxHash,
    ) -> Option<(&[SignedOps<Unverified, StandardMode>], &BlockChannelTx)> {
        self.block_txs.values().find_map(|block| {
            block
                .channel_txs
                .iter()
                .find(|tx| tx.tx_hash() == Some(tx_hash))
                .map(|tx| (block.signed_txs.as_slice(), tx))
        })
    }
}

#[cfg(test)]
mod tests {
    use lb_common_http_client::Slot;
    use lb_groth16::Fr;

    use super::*;
    use crate::test_support::header_id;

    fn process(blocks: &mut BlockStates, block: HeaderId, parent: HeaderId, lib: HeaderId) {
        blocks.insert(block, parent, [], Vec::new(), Vec::new());
        if lib != blocks.lib() {
            blocks.advance_lib(lib, |_| false);
        }
    }

    fn note_id(seed: u64) -> NoteId {
        NoteId::from(Fr::from(seed))
    }

    fn note(seed: u64, value: u64) -> ChannelNote {
        ChannelNote {
            note_id: note_id(seed),
            value,
            pk: Fr::from(seed).into(),
            slot: Slot::from(1),
        }
    }

    fn add(seed: u64, value: u64) -> NoteOp {
        NoteOp::Add(note(seed, value))
    }

    #[test]
    fn notes_view_layers_the_branch_over_the_finalized_set() {
        // LIB holds n1. Branch A spends n1 and adds n2; branch B adds n3 and
        // leaves n1 untouched.
        let genesis = header_id(0);
        let a1 = header_id(1);
        let b1 = header_id(2);
        let mut blocks = BlockStates::new(genesis, MsgId::root());
        blocks.restore_finalized_notes(vec![note(1, 10)]);
        blocks.insert(
            a1,
            genesis,
            [],
            Vec::new(),
            vec![NoteOp::Remove(note_id(1)), add(2, 20)],
        );
        blocks.insert(b1, genesis, [], Vec::new(), vec![add(3, 30)]);

        let at_a = blocks.notes_at(Some(a1));
        assert!(
            at_a.finalized.is_empty(),
            "finalized note spent on branch A"
        );
        assert_eq!(at_a.unfinalized.len(), 1);
        assert_eq!(at_a.unfinalized[0].note_id, note_id(2));

        let at_b = blocks.notes_at(Some(b1));
        assert_eq!(at_b.finalized.len(), 1);
        assert_eq!(at_b.finalized[0].note_id, note_id(1));
        assert_eq!(at_b.unfinalized.len(), 1);
        assert_eq!(at_b.unfinalized[0].note_id, note_id(3));
    }

    #[test]
    fn unfinalized_note_spent_within_the_branch_never_surfaces() {
        let genesis = header_id(0);
        let a1 = header_id(1);
        let a2 = header_id(2);
        let mut blocks = BlockStates::new(genesis, MsgId::root());
        blocks.insert(a1, genesis, [], Vec::new(), vec![add(1, 10)]);
        blocks.insert(a2, a1, [], Vec::new(), vec![NoteOp::Remove(note_id(1))]);

        let view = blocks.notes_at(Some(a2));
        assert_eq!(view.finalized, []);
        assert_eq!(view.unfinalized, []);
    }

    #[test]
    fn finalized_notes_roundtrip_through_restore() {
        let genesis = header_id(0);
        let mut blocks = BlockStates::new(genesis, MsgId::root());
        blocks.restore_finalized_notes(vec![note(1, 10), note(2, 20)]);
        let mut exported = blocks.finalized_notes();
        exported.sort_by_key(|n| n.note_id);

        let mut restored = BlockStates::new(genesis, MsgId::root());
        restored.restore_finalized_notes(exported.clone());
        let mut roundtripped = restored.finalized_notes();
        roundtripped.sort_by_key(|n| n.note_id);
        assert_eq!(exported, roundtripped);
    }

    #[test]
    fn lib_advance_prunes_ancestors_and_orphans() {
        // Chain: genesis <- a1 <- a2 <- a3 (lib) <- a4 <- a5 <- a6
        //                    |
        //                   b1 <- b2 (fork from a1)
        let genesis = header_id(0);
        let a1 = header_id(1);
        let a2 = header_id(2);
        let a3 = header_id(3);
        let a4 = header_id(4);
        let a5 = header_id(5);
        let a6 = header_id(6);
        let b1 = header_id(10);
        let b2 = header_id(11);

        let mut blocks = BlockStates::new(genesis, MsgId::root());

        // Build main chain up to a1
        process(&mut blocks, a1, genesis, genesis);

        // Build fork from a1 (before lib advances past a1)
        process(&mut blocks, b1, a1, genesis);
        process(&mut blocks, b2, b1, genesis);

        // Verify fork blocks exist before lib advances
        assert!(blocks.has_block(&b1));
        assert!(blocks.has_block(&b2));

        // Continue main chain, lib advances to a3
        process(&mut blocks, a2, a1, genesis);
        process(&mut blocks, a3, a2, a3); // lib advances to a3

        // After lib advances to a3:
        // - genesis, a1, a2 should be pruned (ancestors up to and including old lib)
        // - b1, b2 should be GC'd (orphans - their ancestor a1 was pruned)
        // - a3 (new lib) should exist

        assert!(
            !blocks.has_block(&genesis),
            "genesis (old lib) should be pruned"
        );
        assert!(!blocks.has_block(&a1), "a1 should be pruned");
        assert!(!blocks.has_block(&a2), "a2 should be pruned");
        assert!(!blocks.has_block(&b1), "orphan b1 should be pruned");
        assert!(!blocks.has_block(&b2), "orphan b2 should be pruned");

        assert!(blocks.has_block(&a3), "lib should exist");

        // Continue and verify pruning continues working
        process(&mut blocks, a4, a3, a3);
        process(&mut blocks, a5, a4, a5); // lib advances to a5
        process(&mut blocks, a6, a5, a5);

        assert!(!blocks.has_block(&a3), "old lib should be pruned");
        assert!(!blocks.has_block(&a4), "a4 should be pruned");
        assert!(blocks.has_block(&a5), "new lib should exist");
        assert!(blocks.has_block(&a6), "tip should exist");
    }
}
