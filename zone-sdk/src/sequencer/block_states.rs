//! The unfinalized L1 window as the channel sees it: every block above LIB
//! with its parent link, its cumulative safe set, note set and channel
//! lineage, and its classified channel content, plus the finalized tips and
//! note set the branch reads fall back to.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
};

use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps,
        ledger::{NoteId, verification_mode::StandardMode},
        ops::channel::MsgId,
        traits::Hashable as _,
        transactions::{hash::TxHash, states::Unverified},
    },
};
use rpds::{HashTrieMapSync, HashTrieSetSync, ListSync};
use tracing::debug;

use super::{
    TARGET,
    types::{AtomicWithdrawInfo, ChannelNote, ChannelWalletView, InscriptionInfo, PinDepositInfo},
};

type NoteSet = HashTrieMapSync<NoteId, ChannelNote>;
pub type SafeSet = HashTrieSetSync<TxHash>;
/// A channel tx's bytes, owned once and shared between the block store, the
/// pending set and the lineage.
pub type SharedTx = Arc<SignedOps<Unverified, StandardMode>>;

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
    safe_set: SafeSet,
    /// Channel notes alive at this block.
    notes: NoteSet,
    /// The channel lineage on the branch through this block.
    lineage: Lineage,
}

/// A message-lineage position: an entry's id and its lineage-parent.
#[derive(Debug, Clone, Copy)]
struct MsgTip {
    this_msg: MsgId,
    parent_msg: MsgId,
}

/// One lineage entry: a stored channel tx and the position of the entry
/// among its message or config entries.
#[derive(Debug, Clone)]
struct LineageEntry {
    tx: Arc<BlockChannelTx>,
    index: usize,
    config: bool,
}

impl LineageEntry {
    fn info(&self) -> &InscriptionInfo {
        if self.config {
            &self.tx.config_entries()[self.index]
        } else {
            &self.tx.infos()[self.index]
        }
    }
}

/// The channel lineage at a block, above the LIB block's parent: the tips
/// of both lineages and every entry, shared with the parent block.
#[derive(Debug, Clone, Default)]
struct Lineage {
    /// The newest message entry, `None` when the window has none.
    msg_tip: Option<MsgTip>,
    /// The newest config entry, `None` when the window has none.
    config_tip: Option<MsgId>,
    /// Message and config entries in op order, newest first.
    entries: ListSync<LineageEntry>,
}

impl Lineage {
    /// The lineage after `channel_txs` on top of `parent`, or on top of an
    /// empty window when the parent is unknown.
    fn extend(parent: Option<&Self>, channel_txs: &[Arc<BlockChannelTx>]) -> Self {
        let mut lineage = parent.cloned().unwrap_or_default();
        for tx in channel_txs {
            for (index, info) in tx.infos().iter().enumerate() {
                lineage.msg_tip = Some(MsgTip {
                    this_msg: info.this_msg,
                    parent_msg: info.parent_msg,
                });
                lineage.entries = lineage.entries.push_front(LineageEntry {
                    tx: Arc::clone(tx),
                    index,
                    config: false,
                });
            }
            for (index, info) in tx.config_entries().iter().enumerate() {
                lineage.config_tip = Some(info.this_msg);
                lineage.entries = lineage.entries.push_front(LineageEntry {
                    tx: Arc::clone(tx),
                    index,
                    config: true,
                });
            }
        }
        lineage
    }
}

/// The finalized side of the channel: what branch reads fall back to when
/// the unfinalized window has nothing newer.
#[derive(Debug)]
struct Finalized {
    /// Last finalized channel tip — used as parent when pending is empty.
    msg: MsgId,
    /// Lineage-parent of the entry behind [`Self::msg`] — the finalized
    /// entry is matched as a `(this_msg, parent_msg)` pair. `None` when the
    /// finalized entry is unknown (fresh state or checkpoint restore); the
    /// finalized-prefix search then matches nothing (see
    /// `TxState::finalized_prefix_ids`).
    parent_msg: Option<MsgId>,
    /// The config-lineage tip at LIB — the newest config finalized so far, or
    /// [`MsgId::root`] when none has finalized (or is unknown after a
    /// checkpoint restore). Seeds the landable set in
    /// `TxState::shed_stale_pending_configs` so a pending config chaining on
    /// the finalized config tip is not falsely orphaned.
    config: MsgId,
    /// The note set at the newest finalized block: the LIB block's set once
    /// LIB moves there, or a backfilled finalized block's set (see
    /// [`BlockStates::set_finalized_notes_at`]). Splits a branch's notes into
    /// finalized and unfinalized, seeds a parentless block and is exported
    /// for checkpointing.
    notes: NoteSet,
}

#[derive(Debug)]
pub struct BlockStates {
    /// Per-block cumulative state.
    block_states: BTreeMap<HeaderId, BlockState>,
    /// Block parent relationships for pruning.
    parent_map: HashMap<HeaderId, HeaderId>,
    /// Current LIB for pruning.
    current_lib: HeaderId,
    /// Per L1 block channel txs (unfinalized window), canonical or not; the
    /// lineage entries point into them and pending entries share their bytes.
    block_txs: HashMap<HeaderId, Vec<Arc<BlockChannelTx>>>,
    finalized: Finalized,
}

/// A channel-touching tx with its tip-advancing content, classified once at
/// block scan and stored per block.
#[derive(Debug, Clone)]
pub struct BlockChannelTx {
    pub tx: SharedTx,
    pub kind: ChannelTxKind,
}

/// The classified shape of a [`BlockChannelTx`].
#[derive(Debug, Clone)]
pub enum ChannelTxKind {
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
    /// custom-built txs). Updates hand the tx back whole to the caller's own
    /// recovery logic, along with its inscriptions in op order
    /// (`message_entries`) and any `ChannelConfig` ops it carries
    /// (`config_entries`), which sit on the separate config lineage and never
    /// advance the message tip.
    Custom {
        message_entries: Vec<InscriptionInfo>,
        config_entries: Vec<InscriptionInfo>,
    },
}

impl BlockChannelTx {
    /// The tip-advancing entries of this tx, in op order.
    pub fn infos(&self) -> &[InscriptionInfo] {
        match &self.kind {
            ChannelTxKind::Inscription(i) => std::slice::from_ref(i),
            ChannelTxKind::AtomicWithdraw(a) => std::slice::from_ref(&a.inscription),
            ChannelTxKind::PinDeposit(a) => std::slice::from_ref(&a.inscription),
            ChannelTxKind::Config(_) => &[],
            ChannelTxKind::Custom {
                message_entries, ..
            } => message_entries,
        }
    }

    /// The `ChannelConfig` entries this tx carries, in op order. These are on
    /// the config lineage (`this_msg` = config id, `parent_msg` = config
    /// parent) and do not advance the message tip. The clean
    /// `Inscription`/`AtomicWithdraw` shapes carry none; a pure config is a
    /// [`ChannelTxKind::Config`]; mixed/unknown configs ride in
    /// [`ChannelTxKind::Custom`].
    pub fn config_entries(&self) -> &[InscriptionInfo] {
        match &self.kind {
            ChannelTxKind::Inscription(_)
            | ChannelTxKind::AtomicWithdraw(_)
            | ChannelTxKind::PinDeposit(_) => &[],
            ChannelTxKind::Config(c) => std::slice::from_ref(c),
            ChannelTxKind::Custom { config_entries, .. } => config_entries,
        }
    }

    #[must_use]
    pub fn tx_hash(&self) -> TxHash {
        self.tx.hash()
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
            finalized: Finalized {
                msg: finalized_msg,
                parent_msg: None,
                config: MsgId::root(),
                notes: NoteSet::new_sync(),
            },
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
        self.finalized.msg
    }

    /// The lineage-parent of the finalized channel tip, `None` when unknown.
    #[must_use]
    pub const fn finalized_parent_msg(&self) -> Option<MsgId> {
        self.finalized.parent_msg
    }

    /// Update the finalized channel tip from backfilled finalized history.
    /// `parent` is the entry's lineage-parent; pass `None` only when it is
    /// genuinely unknown (disables the finalized-prefix mask until the next
    /// boundary move records a parent).
    pub const fn set_finalized_msg(&mut self, msg: MsgId, parent: Option<MsgId>) {
        self.finalized.msg = msg;
        self.finalized.parent_msg = parent;
    }

    /// The finalized config-lineage tip (the newest config at/below LIB). Read
    /// for checkpointing; restored via [`Self::set_finalized_config`].
    #[must_use]
    pub const fn finalized_config(&self) -> MsgId {
        self.finalized.config
    }

    /// Restore/refresh the finalized config-lineage tip — from a checkpoint on
    /// warm start, or from backfilled finalized history. Without this the tip
    /// resets to [`MsgId::root`] on restart, and `config_tip_at` can fall back
    /// to a stale parent once the config's block is pruned below LIB.
    pub const fn set_finalized_config(&mut self, config: MsgId) {
        self.finalized.config = config;
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
        let mut notes = parent.map_or_else(|| self.finalized.notes.clone(), |p| p.notes.clone());
        let channel_txs: Vec<Arc<BlockChannelTx>> = channel_txs.into_iter().map(Arc::new).collect();
        let lineage = Lineage::extend(parent.map(|p| &p.lineage), &channel_txs);

        for tx in our_txs {
            safe_set = safe_set.insert(tx);
        }
        for op in note_ops {
            notes = match op {
                NoteOp::Add(note) => notes.insert(note.note_id, note),
                NoteOp::Remove(id) => notes.remove(&id),
            };
        }
        self.block_states.insert(
            block_id,
            BlockState {
                safe_set,
                notes,
                lineage,
            },
        );

        if !channel_txs.is_empty() {
            self.block_txs.insert(block_id, channel_txs);
        }
    }

    /// Move LIB to `lib`: advance the finalized tips and note set, prune the
    /// blocks at or below the old window, drop finalized hashes from the
    /// safe sets and rebase the lineages on the new LIB block. `is_pending`
    /// marks hashes still tracked as pending, which stay in the sets.
    pub fn advance_lib(&mut self, lib: HeaderId, is_pending: impl Fn(&TxHash) -> bool) {
        // The new LIB block's tips are the finalized ones; the (id, parent)
        // pair identifies the finalized position in `finalized_prefix_ids`.
        // A window without entries keeps the previous tips.
        if let Some(state) = self.block_states.get(&lib) {
            if let Some(tip) = state.lineage.msg_tip {
                self.finalized.msg = tip.this_msg;
                self.finalized.parent_msg = Some(tip.parent_msg);
            }
            if let Some(config) = state.lineage.config_tip {
                self.finalized.config = config;
            }
            self.finalized.notes = state.notes.clone();
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
        self.rebase_lineages();
    }

    /// Rebuild every block's lineage from the LIB block's own entries up, so
    /// the entries of pruned ancestors are released. Children extend their
    /// rebuilt parent, which keeps the sharing between forks.
    fn rebase_lineages(&mut self) {
        let mut children: HashMap<HeaderId, Vec<HeaderId>> = HashMap::new();
        for (&block, &parent) in &self.parent_map {
            if block != self.current_lib {
                children.entry(parent).or_default().push(block);
            }
        }

        let mut queue = VecDeque::from([(self.current_lib, None)]);
        while let Some((block, parent)) = queue.pop_front() {
            let txs = self.block_txs.get(&block).map_or(&[][..], Vec::as_slice);
            let lineage = Lineage::extend(parent.as_ref(), txs);
            if let Some(state) = self.block_states.get_mut(&block) {
                state.lineage = lineage.clone();
            }
            for &child in children.get(&block).into_iter().flatten() {
                queue.push_back((child, Some(lineage.clone())));
            }
        }
    }

    /// Make `block`'s note set the finalized one. Backfilled finalized blocks
    /// are inserted without moving LIB, so they mark it explicitly. A block
    /// that does not chain from LIB is stale (its ancestors were pruned by a
    /// LIB move) and leaves the set alone.
    pub fn set_finalized_notes_at(&mut self, block: HeaderId) {
        if !self.descends_from_lib(block) {
            debug!(target: TARGET, ?block, "Stale finalized block, notes left as they are");
            return;
        }
        if let Some(state) = self.block_states.get(&block) {
            self.finalized.notes = state.notes.clone();
        }
    }

    fn descends_from_lib(&self, block: HeaderId) -> bool {
        let mut current = block;
        loop {
            if current == self.current_lib {
                return true;
            }
            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => return false,
            }
        }
    }

    /// The channel's note set at `tip`, split into the notes also in the
    /// finalized set and the rest; the finalized set alone when `tip` is
    /// `None` or unknown.
    pub fn notes_at(&self, tip: Option<HeaderId>) -> ChannelWalletView {
        let notes = tip
            .and_then(|tip| self.block_states.get(&tip))
            .map_or(&self.finalized.notes, |state| &state.notes);
        let (finalized, unfinalized): (Vec<_>, Vec<_>) = notes
            .values()
            .cloned()
            .partition(|note| self.finalized.notes.contains_key(&note.note_id));
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
        self.finalized.notes.get(id).or_else(|| {
            self.block_states
                .values()
                .find_map(|state| state.notes.get(id))
        })
    }

    /// The finalized note set, for checkpointing.
    pub fn finalized_notes(&self) -> Vec<ChannelNote> {
        self.finalized.notes.values().cloned().collect()
    }

    /// Restore the finalized note set from a checkpoint; the LIB block
    /// starts from it.
    pub fn restore_finalized_notes(&mut self, notes: Vec<ChannelNote>) {
        let notes: NoteSet = notes.into_iter().map(|n| (n.note_id, n)).collect();
        if let Some(state) = self.block_states.get_mut(&self.current_lib) {
            state.notes = notes.clone();
        }
        self.finalized.notes = notes;
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

    /// Tx hashes mined on `tip`'s branch (its cumulative safe set); empty at
    /// an unknown tip.
    pub fn safe_at(&self, tip: HeaderId) -> SafeSet {
        self.block_states
            .get(&tip)
            .map(|s| s.safe_set.clone())
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

    fn lineage_at(&self, block_id: HeaderId) -> Option<&Lineage> {
        self.block_states.get(&block_id).map(|state| &state.lineage)
    }

    /// The channel tip at a block: the newest message entry on its branch
    /// within the unfinalized window, or [`Self::finalized_msg`] when the
    /// window has none.
    #[must_use]
    pub fn channel_tip_at(&self, block_id: HeaderId) -> MsgId {
        self.lineage_at(block_id)
            .and_then(|lineage| lineage.msg_tip)
            .map_or(self.finalized.msg, |tip| tip.this_msg)
    }

    /// The config-lineage tip at a block: the newest config on its branch
    /// within the unfinalized window, or [`Self::finalized_config`] when the
    /// window has none. Derived only from blocks we have processed, so —
    /// unlike the node's single config tip — it never races ahead of local
    /// state.
    #[must_use]
    pub fn config_tip_at(&self, block_id: HeaderId) -> MsgId {
        self.lineage_at(block_id)
            .and_then(|lineage| lineage.config_tip)
            .unwrap_or(self.finalized.config)
    }

    /// All tip-advancing entries on a branch back to LIB, oldest first:
    /// message entries and config entries, in op order.
    pub fn infos_on_branch(&self, tip: HeaderId) -> Vec<InscriptionInfo> {
        self.lineage_at(tip).map_or_else(Vec::new, |lineage| {
            let mut entries: Vec<_> = lineage
                .entries
                .iter()
                .map(|entry| entry.info().clone())
                .collect();
            entries.reverse();
            entries
        })
    }

    /// Channel txs on `tip`'s branch strictly above LIB, oldest block first.
    pub fn channel_txs_on_branch(
        &self,
        tip: HeaderId,
    ) -> impl Iterator<Item = &Arc<BlockChannelTx>> {
        self.branch_blocks_above_lib(tip)
            .into_iter()
            .filter_map(|id| self.block_txs.get(&id))
            .flatten()
    }

    /// A mined channel tx by hash.
    pub fn channel_tx(&self, tx_hash: TxHash) -> Option<&BlockChannelTx> {
        self.block_txs
            .values()
            .flatten()
            .find(|tx| tx.tx_hash() == tx_hash)
            .map(|tx| &**tx)
    }
}

#[cfg(test)]
mod tests {
    use lb_common_http_client::Slot;
    use lb_core::mantle::{Op, ops::channel::ChannelId};
    use lb_groth16::Fr;

    use super::*;
    use crate::test_support::{header_id, inscribe_op, unverified_tx_with_ops};

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

    fn msg_id(n: u8) -> MsgId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        MsgId::from(bytes)
    }

    fn entry(parent: u8, this: u8) -> InscriptionInfo {
        InscriptionInfo {
            tx_hash: TxHash([this; 32]),
            parent_msg: msg_id(parent),
            this_msg: msg_id(this),
            payload: [].into(),
            signer: None,
        }
    }

    fn stored(kind: ChannelTxKind) -> BlockChannelTx {
        let op = inscribe_op(ChannelId::from([0; 32]), MsgId::root(), b"");
        BlockChannelTx {
            tx: Arc::new(unverified_tx_with_ops(vec![Op::ChannelInscribe(op)])),
            kind,
        }
    }

    fn inscription(parent: u8, this: u8) -> BlockChannelTx {
        stored(ChannelTxKind::Inscription(entry(parent, this)))
    }

    fn config(parent: u8, this: u8) -> BlockChannelTx {
        stored(ChannelTxKind::Config(entry(parent, this)))
    }

    fn ids(infos: Vec<InscriptionInfo>) -> Vec<MsgId> {
        infos.into_iter().map(|i| i.this_msg).collect()
    }

    #[test]
    fn lineage_reads_follow_the_branch() {
        // genesis <- a1 [m1] <- a2 [c11] <- a3 []
        //                   <- b1 [m2]
        let genesis = header_id(0);
        let (a1, a2, a3, b1) = (header_id(1), header_id(2), header_id(3), header_id(10));
        let mut blocks = BlockStates::new(genesis, MsgId::root());
        blocks.insert(a1, genesis, [], vec![inscription(0, 1)], Vec::new());
        blocks.insert(a2, a1, [], vec![config(0, 11)], Vec::new());
        blocks.insert(a3, a2, [], Vec::new(), Vec::new());
        blocks.insert(b1, a1, [], vec![inscription(1, 2)], Vec::new());

        assert_eq!(blocks.channel_tip_at(a3), msg_id(1));
        assert_eq!(blocks.config_tip_at(a3), msg_id(11));
        assert_eq!(ids(blocks.infos_on_branch(a3)), [msg_id(1), msg_id(11)]);

        assert_eq!(blocks.channel_tip_at(b1), msg_id(2));
        assert_eq!(blocks.config_tip_at(b1), MsgId::root());
        assert_eq!(ids(blocks.infos_on_branch(b1)), [msg_id(1), msg_id(2)]);
    }

    #[test]
    fn lib_advance_finalizes_the_tips_and_rebases_the_lineages() {
        // genesis <- a1 [m1] <- a2 [c11] <- a3 [m2]; LIB moves to a2.
        let genesis = header_id(0);
        let (a1, a2, a3) = (header_id(1), header_id(2), header_id(3));
        let mut blocks = BlockStates::new(genesis, MsgId::root());
        blocks.insert(a1, genesis, [], vec![inscription(0, 1)], Vec::new());
        blocks.insert(a2, a1, [], vec![config(0, 11)], Vec::new());
        blocks.insert(a3, a2, [], vec![inscription(1, 2)], Vec::new());
        blocks.advance_lib(a2, |_| false);

        assert_eq!(blocks.finalized_msg(), msg_id(1));
        assert_eq!(blocks.finalized_parent_msg(), Some(MsgId::root()));
        assert_eq!(blocks.finalized_config(), msg_id(11));

        assert_eq!(
            blocks.channel_tip_at(a2),
            msg_id(1),
            "the finalized tip serves a window without message entries"
        );
        assert_eq!(ids(blocks.infos_on_branch(a2)), [msg_id(11)]);
        assert_eq!(
            ids(blocks.infos_on_branch(a3)),
            [msg_id(11), msg_id(2)],
            "entries below LIB leave the branch"
        );
    }

    #[test]
    fn parentless_block_reads_only_its_own_entries() {
        let genesis = header_id(0);
        let (a1, x, y) = (header_id(1), header_id(2), header_id(3));
        let unknown = header_id(99);
        let mut blocks = BlockStates::new(genesis, MsgId::root());
        blocks.insert(a1, genesis, [], vec![inscription(0, 1)], Vec::new());
        blocks.insert(x, unknown, [], vec![inscription(1, 2)], Vec::new());
        blocks.insert(y, unknown, [], Vec::new(), Vec::new());

        assert_eq!(blocks.channel_tip_at(x), msg_id(2));
        assert_eq!(ids(blocks.infos_on_branch(x)), [msg_id(2)]);
        assert_eq!(blocks.channel_tip_at(y), MsgId::root());
        assert!(blocks.infos_on_branch(y).is_empty());
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
    fn stale_backfilled_block_does_not_refinalize_a_spent_note() {
        // G <- a1 (adds n1) <- a2 (spends n1); LIB moves to a2. A late
        // backfill batch re-inserts a1 under the pruned G.
        let genesis = header_id(0);
        let a1 = header_id(1);
        let a2 = header_id(2);
        let mut blocks = BlockStates::new(genesis, MsgId::root());
        blocks.insert(a1, genesis, [], Vec::new(), vec![add(1, 10)]);
        blocks.insert(a2, a1, [], Vec::new(), vec![NoteOp::Remove(note_id(1))]);
        blocks.advance_lib(a2, |_| false);
        assert_eq!(blocks.finalized_notes(), []);

        blocks.insert(a1, genesis, [], Vec::new(), vec![add(1, 10)]);
        blocks.set_finalized_notes_at(a1);
        assert_eq!(blocks.finalized_notes(), []);
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
