use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use lb_core::{
    header::HeaderId,
    mantle::{
        SignedMantleTx, Transaction as _,
        ops::{
            Op,
            channel::{ChannelId, MsgId, inscribe::Inscription},
        },
        transactions::TxHash,
    },
};
use rpds::HashTrieSetSync;

use super::types::{
    AtomicWithdrawInfo, ChannelUpdateTx, InscriptionInfo, PendingTx, TxSource, WithdrawInfo,
};

/// Result of channel update detection — the linear block-level delta
/// between two canonical chains.
///
/// - `orphaned`: txs on blocks of the old canonical chain that are not on
///   blocks of the new canonical chain. Revert from state.
/// - `adopted`: txs on blocks of the new canonical chain that are not on blocks
///   of the old canonical chain. Apply to state.
/// - When `orphaned` is empty, this is an extension-only update.
#[derive(Debug)]
pub struct ChannelUpdateInfo {
    /// Txs removed from the canonical chain (revert from state).
    pub orphaned: Vec<ChannelUpdateTx>,
    /// Txs added to the canonical chain (apply to state).
    pub adopted: Vec<ChannelUpdateTx>,
    /// The new channel tip `MsgId`.
    pub new_channel_tip: MsgId,
}

/// `first_parent` is `None` for config-led txs (a config resets the tip, so
/// the tx is always mineable) — such entries are never shed.
#[derive(Debug, Clone)]
struct PendingOtherTx {
    signed_tx: SignedMantleTx,
    first_parent: Option<MsgId>,
    last_msg: Option<MsgId>,
}

fn opaque_lineage(tx: &SignedMantleTx, channel_id: ChannelId) -> (Option<MsgId>, Option<MsgId>) {
    let mut first_parent = None;
    let mut first_seen = false;
    let mut last_msg = None;
    for op in tx.mantle_tx.ops() {
        match op {
            Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                if !first_seen {
                    first_seen = true;
                    first_parent = Some(inscribe.parent);
                }
                last_msg = Some(inscribe.id());
            }
            Op::ChannelConfig(config) if config.channel == channel_id => {
                first_seen = true;
                last_msg = Some(config.id());
            }
            _ => {}
        }
    }
    (first_parent, last_msg)
}

/// Local pending inscription with lineage metadata.
///
/// `withdraws == None` is a plain inscription; `Some(_)` is an atomic
/// inscription+withdraw bundle. The bundle nature lets us surface the right
/// [`PendingTx`] variant on finalize/adopt and re-prepare on orphan.
#[derive(Debug, Clone)]
pub struct PendingInscription {
    pub tx_hash: TxHash,
    pub signed_tx: SignedMantleTx,
    pub parent_msg: MsgId,
    pub this_msg: MsgId,
    pub payload: Inscription,
    pub withdraws: Option<Vec<WithdrawInfo>>,
    pub posted: bool,
}

/// Transaction state tracker.
pub struct TxState {
    /// Local pending inscriptions indexed by tx hash.
    pending: HashMap<TxHash, PendingInscription>,
    /// Reverse index: parent `MsgId` → tx hashes that chain from it.
    pending_by_parent: HashMap<MsgId, Vec<TxHash>>,
    /// Opaque pending txs (`channel_config`, raw `submit_signed_tx`):
    /// retried byte-identically until finalized or shed.
    pending_other: HashMap<TxHash, PendingOtherTx>,
    /// Bounded insertion-ordered tx hashes accepted locally by this sequencer
    /// runtime or restored from its checkpoint.
    local_txs: VecDeque<TxHash>,
    /// Per-block cumulative safe sets.
    block_states: BTreeMap<HeaderId, HashTrieSetSync<TxHash>>,
    /// Block parent relationships for pruning.
    parent_map: HashMap<HeaderId, HeaderId>,
    /// Current LIB for pruning.
    current_lib: HeaderId,
    /// Channel-touching txs per L1 block (unfinalized window only),
    /// classified at block scan by `block_fetch::classify_channel_txs`.
    block_txs: HashMap<HeaderId, Vec<BlockChannelTx>>,
    /// Last finalized channel tip — used as parent when pending is empty.
    finalized_msg: MsgId,
}

/// A channel-touching tx's tip-advancing content, classified once at block
/// scan and stored per block.
#[derive(Debug, Clone)]
pub enum BlockChannelTx {
    /// `publish` shape: a single inscription.
    Inscription(InscriptionInfo),
    /// `publish_atomic_withdraw` shape: an inscription + its withdraws.
    AtomicWithdraw(AtomicWithdrawInfo),
    /// `channel_config` shape: a synthetic tip-reset entry (empty payload).
    Config(InscriptionInfo),
    /// A shape the SDK cannot produce (bundled deposits, multi-inscribe,
    /// custom-built txs). Kept whole — updates hand the tx back to the
    /// caller's own recovery logic — along with its tip-advancing entries
    /// in op order.
    Custom {
        tx: SignedMantleTx,
        entries: Vec<InscriptionInfo>,
    },
}

impl BlockChannelTx {
    /// The tip-advancing entries of this tx, in op order.
    pub fn infos(&self) -> &[InscriptionInfo] {
        match self {
            Self::Inscription(i) | Self::Config(i) => std::slice::from_ref(i),
            Self::AtomicWithdraw(a) => std::slice::from_ref(&a.inscription),
            Self::Custom { entries, .. } => entries,
        }
    }

    /// The channel tip after this tx (its last tip-advancing op).
    fn tip_msg(&self) -> Option<MsgId> {
        self.infos().last().map(|i| i.this_msg)
    }

    #[must_use]
    pub fn tx_hash(&self) -> Option<TxHash> {
        self.infos().first().map(|i| i.tx_hash)
    }
}

impl TxState {
    #[must_use]
    pub fn new(lib: HeaderId, finalized_msg: MsgId) -> Self {
        let mut block_states = BTreeMap::new();
        block_states.insert(lib, HashTrieSetSync::new_sync());
        Self {
            pending: HashMap::new(),
            pending_by_parent: HashMap::new(),
            pending_other: HashMap::new(),
            local_txs: VecDeque::new(),
            block_states,
            parent_map: HashMap::new(),
            current_lib: lib,
            block_txs: HashMap::new(),
            finalized_msg,
        }
    }

    /// Update the finalized channel tip from backfilled finalized history.
    pub const fn set_finalized_msg(&mut self, msg: MsgId) {
        self.finalized_msg = msg;
    }

    /// Submit an inscription tx for tracking with lineage metadata. Use
    /// [`Self::submit_atomic_withdraw`] for inscription+withdraw bundles.
    pub fn submit_inscription(
        &mut self,
        signed_tx: SignedMantleTx,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
    ) {
        self.insert_pending(signed_tx, parent_msg, this_msg, payload, None);
    }

    /// Submit an atomic inscription+withdraw bundle for tracking. `withdraws`
    /// must mirror the `Op::ChannelWithdraw` ops in the bundle, in tx order.
    pub fn submit_atomic_withdraw(
        &mut self,
        signed_tx: SignedMantleTx,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        withdraws: Vec<WithdrawInfo>,
    ) {
        self.insert_pending(signed_tx, parent_msg, this_msg, payload, Some(withdraws));
    }

    fn insert_pending(
        &mut self,
        signed_tx: SignedMantleTx,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        withdraws: Option<Vec<WithdrawInfo>>,
    ) {
        let tx_hash = signed_tx.mantle_tx.hash();
        self.track_local_tx(tx_hash);
        self.pending_by_parent
            .entry(parent_msg)
            .or_default()
            .push(tx_hash);
        self.pending.insert(
            tx_hash,
            PendingInscription {
                tx_hash,
                signed_tx,
                parent_msg,
                this_msg,
                payload,
                withdraws,
                posted: false,
            },
        );
    }

    /// Track an inscription observed on the canonical channel (ours or
    /// another sequencer's) so the pending set mirrors the channel view
    /// above LIB: a reorged-out entry whose lineage still reaches the
    /// channel tip is retried byte-identically via [`Self::pending_txs`],
    /// no matter who authored it. No-op when the tx is already tracked.
    ///
    /// `withdraws` mirrors the tx's `ChannelWithdraw` ops (an atomic
    /// inscription+withdraw bundle), matching [`Self::submit_atomic_withdraw`]
    /// classification. Observed entries start `posted` — they were seen on
    /// chain, so they never count as first-time publishes.
    pub fn observe_channel_inscription(
        &mut self,
        signed_tx: SignedMantleTx,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        withdraws: Option<Vec<WithdrawInfo>>,
    ) {
        let tx_hash = signed_tx.mantle_tx.hash();
        if self.is_tracked(&tx_hash) {
            return;
        }
        self.pending_by_parent
            .entry(parent_msg)
            .or_default()
            .push(tx_hash);
        self.pending.insert(
            tx_hash,
            PendingInscription {
                tx_hash,
                signed_tx,
                parent_msg,
                this_msg,
                payload,
                withdraws,
                posted: true,
            },
        );
    }

    /// Whether the tx is tracked in either pending map.
    #[must_use]
    pub fn is_tracked(&self, tx_hash: &TxHash) -> bool {
        self.pending.contains_key(tx_hash) || self.pending_other.contains_key(tx_hash)
    }

    /// Tx hashes currently tracked in either pending map.
    #[must_use]
    pub fn tracked_tx_hashes(&self) -> HashSet<TxHash> {
        self.pending
            .keys()
            .chain(self.pending_other.keys())
            .copied()
            .collect()
    }

    pub fn submit_other(&mut self, signed_tx: SignedMantleTx, channel_id: ChannelId) {
        let tx_hash = signed_tx.mantle_tx.hash();
        let (first_parent, last_msg) = opaque_lineage(&signed_tx, channel_id);
        self.track_local_tx(tx_hash);
        self.pending_other.insert(
            tx_hash,
            PendingOtherTx {
                signed_tx,
                first_parent,
                last_msg,
            },
        );
    }

    fn track_local_tx(&mut self, tx_hash: TxHash) {
        if !self.local_txs.contains(&tx_hash) {
            self.local_txs.push_back(tx_hash);
        }
    }

    pub fn prune_local_tx_tracking(&mut self, max_tracked: usize) {
        while self.local_txs.len() > max_tracked {
            self.local_txs.pop_front();
        }
    }

    pub fn remove_local_tx(&mut self, tx_hash: &TxHash) {
        self.local_txs.retain(|tracked| tracked != tx_hash);
    }

    /// Process a new block. Finalization is handled by backfill ground
    /// truth, not by the safe-set walk here.
    pub fn process_block(
        &mut self,
        block_id: HeaderId,
        parent_id: HeaderId,
        lib: HeaderId,
        our_txs: impl IntoIterator<Item = TxHash>,
        channel_txs: Vec<BlockChannelTx>,
    ) {
        // Store parent relationship for pruning
        self.parent_map.insert(block_id, parent_id);

        // Build cumulative safe set from parent. Parent may be missing
        // when blocks are processed from slot-range backfill and LIB has
        // advanced between batches (pruning the parent). Starting with an
        // empty set is conservative: txs show as "pending" until seen in
        // a subsequent block with a known parent.
        let mut safe_set = self
            .block_states
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();

        for tx in our_txs {
            if self.pending.contains_key(&tx) || self.pending_other.contains_key(&tx) {
                safe_set = safe_set.insert(tx);
            }
        }
        self.block_states.insert(block_id, safe_set);

        // Store the block's classified channel txs
        if !channel_txs.is_empty() {
            self.block_txs.insert(block_id, channel_txs);
        }

        // When lib advances: update finalized_msg and prune.
        // NOTE: we do NOT remove pending txs here. Pending txs are only
        // removed when confirmed by backfill ground truth (canonical
        // finalized blocks from the node). The safe set is used for
        // branch-relative status (pending_txs resubmission) but not
        // as proof of canonical finalization — it can include blocks
        // from orphaned branches in concurrent scenarios.
        if lib != self.current_lib {
            // Compute finalized_msg BEFORE pruning — walk from new LIB
            // backwards to find the latest inscription in the finalized range.
            self.finalized_msg = self.channel_tip_at(lib);

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
            if let Some(lib_safe_set) = self.block_states.get(&lib) {
                let finalized_hashes: Vec<TxHash> = lib_safe_set
                    .iter()
                    .filter(|hash| {
                        !self.pending.contains_key(hash) && !self.pending_other.contains_key(hash)
                    })
                    .copied()
                    .collect();
                for safe_set in self.block_states.values_mut() {
                    for tx_hash in &finalized_hashes {
                        *safe_set = safe_set.remove(tx_hash);
                    }
                }
            }

            self.prune_orphans(lib);
            self.current_lib = lib;
        }
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

    /// Pending txs eligible for resubmission: not yet safe at tip AND
    /// part of the local suffix reachable from canonical channel tip.
    ///
    /// Returned in parent-before-child order (BFS from channel tip via
    /// `pending_by_parent`) so the node's mempool sees the parent before
    /// any child — matters on checkpoint resume, where `HashMap`
    /// iteration order is arbitrary.
    pub fn pending_txs(&self, tip: HeaderId) -> Vec<(TxHash, SignedMantleTx)> {
        let safe = self
            .block_states
            .get(&tip)
            .cloned()
            .unwrap_or_else(HashTrieSetSync::new_sync);

        let channel_tip = self.channel_tip_at(tip);
        let inscriptions = self
            .collect_pending_suffix(channel_tip)
            .into_iter()
            .filter(|info| !safe.contains(&info.tx_hash))
            .filter_map(|info| {
                self.pending
                    .get(&info.tx_hash)
                    .map(|p| (info.tx_hash, p.signed_tx.clone()))
            });
        let others = self
            .pending_other
            .iter()
            .filter(|(hash, _)| !safe.contains(hash))
            .map(|(hash, entry)| (*hash, entry.signed_tx.clone()));
        inscriptions.chain(others).collect()
    }

    /// Number of pending transactions (all types).
    #[cfg(test)]
    #[must_use]
    pub fn unfinalized_count(&self) -> usize {
        self.pending.len() + self.pending_other.len()
    }

    /// Number of pending channel inscription transactions.
    #[must_use]
    pub fn pending_publish_count(&self) -> usize {
        self.pending.len()
    }

    /// Number of pending channel inscription transactions already posted by
    /// this runtime.
    #[must_use]
    pub fn posted_pending_publish_count(&self) -> usize {
        self.pending.values().filter(|p| p.posted).count()
    }

    /// Whether there are pending channel inscriptions.
    #[must_use]
    pub fn has_pending_inscriptions(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Remove pending inscriptions whose lineage does NOT reach the current
    /// channel tip and that aren't already in a block on this branch.
    /// Returns the removed entries in **parent-before-child (BFS) order** so
    /// a consumer that iterates and republishes naturally rebuilds the chain
    /// in dependency order. Keeps `self.pending` linear.
    ///
    /// Bundle-aware: atomic inscription+withdraw bundles are returned as
    /// [`PendingTx::AtomicWithdraw`] so the caller can re-prepare them with
    /// fresh `parent_msg` + `withdraw_nonce`; plain inscriptions are returned
    /// as [`PendingTx::Inscription`].
    pub fn shed_off_branch_pending(&mut self, tip: HeaderId) -> Vec<PendingTx> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let channel_tip = self.channel_tip_at(tip);
        let on_branch: HashSet<TxHash> = self
            .collect_pending_suffix(channel_tip)
            .iter()
            .map(|i| i.tx_hash)
            .collect();
        let safe: HashSet<TxHash> = self
            .block_states
            .get(&tip)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();

        let eligible: HashSet<TxHash> = self
            .pending
            .keys()
            .filter(|h| !on_branch.contains(h) && !safe.contains(h))
            .copied()
            .collect();
        if eligible.is_empty() {
            return Vec::new();
        }

        // Find root parents: parent_msg values for eligible entries whose
        // parent is NOT the `this_msg` of another eligible entry. Sort for
        // determinism across HashMap iteration order.
        let eligible_this_msgs: HashSet<MsgId> = eligible
            .iter()
            .filter_map(|h| self.pending.get(h).map(|p| p.this_msg))
            .collect();
        let mut root_parents: Vec<MsgId> = eligible
            .iter()
            .filter_map(|h| {
                let p = self.pending.get(h)?;
                if eligible_this_msgs.contains(&p.parent_msg) {
                    None
                } else {
                    Some(p.parent_msg)
                }
            })
            .collect();
        root_parents.sort_by_key(|m| <[u8; 32]>::from(*m));
        root_parents.dedup();

        // BFS from each root parent via pending_by_parent; collect only
        // eligible entries in parent-first order.
        let mut ordered = Vec::with_capacity(eligible.len());
        let mut seen = HashSet::new();
        for root in root_parents {
            for info in self.collect_pending_suffix(root) {
                if eligible.contains(&info.tx_hash) && seen.insert(info.tx_hash) {
                    let tx_hash = info.tx_hash;
                    let entry = match self
                        .pending
                        .get(&tx_hash)
                        .and_then(|p| p.withdraws.as_ref())
                    {
                        Some(withdraws) => PendingTx::AtomicWithdraw(AtomicWithdrawInfo {
                            tx_hash,
                            inscription: info,
                            withdraws: withdraws.clone(),
                        }),
                        None => PendingTx::Inscription(info),
                    };
                    ordered.push(entry);
                }
            }
        }

        for entry in &ordered {
            self.remove_pending(&entry.tx_hash());
        }
        ordered
    }

    /// Shed pending opaque txs whose first inscription's parent slot was
    /// consumed by a conflicting entry: removed from retry and returned
    /// whole for orphan reporting.
    pub fn shed_off_branch_pending_other(&mut self, tip: HeaderId) -> Vec<SignedMantleTx> {
        if self.pending_other.is_empty() {
            return Vec::new();
        }
        let channel_tip = self.channel_tip_at(tip);
        let mut landable: HashSet<MsgId> = self
            .collect_pending_suffix(channel_tip)
            .iter()
            .map(|info| info.this_msg)
            .collect();
        landable.insert(channel_tip);
        // A viable entry makes its own last message landable, so entries
        // chained on it are kept too.
        loop {
            let mut changed = false;
            for entry in self.pending_other.values() {
                let viable = entry
                    .first_parent
                    .is_none_or(|parent| landable.contains(&parent));
                if viable && let Some(last_msg) = entry.last_msg {
                    changed |= landable.insert(last_msg);
                }
            }
            if !changed {
                break;
            }
        }
        let safe: HashSet<TxHash> = self
            .block_states
            .get(&tip)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();

        let mut shed: Vec<TxHash> = self
            .pending_other
            .iter()
            .filter(|(hash, entry)| {
                !safe.contains(*hash)
                    && entry
                        .first_parent
                        .is_some_and(|parent| !landable.contains(&parent))
            })
            .map(|(hash, _)| *hash)
            .collect();
        // Sort for determinism across `HashMap` iteration order.
        shed.sort_unstable_by_key(|hash| hash.0);
        shed.into_iter()
            .filter_map(|hash| self.remove_pending(&hash))
            .collect()
    }

    /// Check if we have state for a block.
    #[must_use]
    pub fn has_block(&self, block_id: &HeaderId) -> bool {
        self.block_states.contains_key(block_id)
    }

    /// Current LIB.
    #[must_use]
    pub const fn lib(&self) -> HeaderId {
        self.current_lib
    }

    /// Look up a pending inscription (or atomic-withdraw bundle) by tx hash.
    /// Used during finalization to capture bundle info (`withdraws`) before
    /// `remove_pending` strips the entry, so finalized events can surface the
    /// correct [`PendingTx`] variant.
    #[must_use]
    pub fn pending_inscription(&self, tx_hash: &TxHash) -> Option<&PendingInscription> {
        self.pending.get(tx_hash)
    }

    #[must_use]
    pub fn tx_source(&self, tx_hash: &TxHash) -> TxSource {
        if self.local_txs.contains(tx_hash) {
            TxSource::Local
        } else {
            TxSource::Other
        }
    }

    /// Mark a pending inscription as posted. Returns true only for the first
    /// successful post in this runtime.
    pub fn mark_pending_inscription_posted(&mut self, tx_hash: &TxHash) -> bool {
        let Some(pending) = self.pending.get_mut(tx_hash) else {
            return false;
        };
        let first_post = !pending.posted;
        pending.posted = true;
        first_post
    }

    /// Whether a non-inscription pending tx is tracked under this hash.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn pending_other_contains(&self, tx_hash: &TxHash) -> bool {
        self.pending_other.contains_key(tx_hash)
    }

    /// All pending transactions (for checkpoint serialization).
    #[must_use]
    pub fn all_pending_txs(&self) -> Vec<(TxHash, SignedMantleTx)> {
        let inscriptions = self
            .pending
            .iter()
            .map(|(hash, p)| (*hash, p.signed_tx.clone()));
        let others = self
            .pending_other
            .iter()
            .map(|(hash, entry)| (*hash, entry.signed_tx.clone()));
        inscriptions.chain(others).collect()
    }

    /// Remove a pending inscription and return its signed tx.
    pub fn remove_pending(&mut self, tx_hash: &TxHash) -> Option<SignedMantleTx> {
        if let Some(removed) = self.pending.remove(tx_hash) {
            if let Some(children) = self.pending_by_parent.get_mut(&removed.parent_msg) {
                children.retain(|h| h != tx_hash);
                if children.is_empty() {
                    self.pending_by_parent.remove(&removed.parent_msg);
                }
            }
            Some(removed.signed_tx)
        } else {
            self.pending_other
                .remove(tx_hash)
                .map(|entry| entry.signed_tx)
        }
    }

    /// Derive the publish parent from state.
    ///
    /// Walks the local pending suffix from canonical tip only if the
    /// lineage is unambiguous (exactly one child at each step).
    /// Falls back to canonical tip if ambiguous or no pending suffix.
    #[must_use]
    pub fn publish_parent(&self, tip: HeaderId) -> MsgId {
        let channel_tip = self.channel_tip_at(tip);
        let tail = self.pending_publish_tail(channel_tip);
        tail.unwrap_or(channel_tip)
    }

    /// Walk local pending lineage from `from_msg` to find the tail,
    /// but ONLY if the chain is strictly linear (one child per parent).
    /// Returns None if no pending children or if lineage branches.
    fn pending_publish_tail(&self, from_msg: MsgId) -> Option<MsgId> {
        let mut current = from_msg;
        let mut found_any = false;

        loop {
            let Some(children) = self.pending_by_parent.get(&current) else {
                return found_any.then_some(current);
            };
            if children.len() != 1 {
                return found_any.then_some(current);
            }
            let Some(pending) = self.pending.get(&children[0]) else {
                return found_any.then_some(current);
            };
            current = pending.this_msg;
            found_any = true;
        }
    }

    /// Derive the channel tip `MsgId` at a given L1 block by walking backwards
    /// through the block tree and finding the most recent inscription.
    /// Returns `finalized_msg` if no inscriptions are found in the
    /// unfinalized window.
    #[must_use]
    pub fn channel_tip_at(&self, block_id: HeaderId) -> MsgId {
        let mut current = block_id;
        loop {
            if let Some(txs) = self.block_txs.get(&current)
                && let Some(tip) = txs.iter().rev().find_map(BlockChannelTx::tip_msg)
            {
                return tip;
            }

            if current == self.current_lib {
                return self.finalized_msg;
            }

            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => return self.finalized_msg,
            }
        }
    }

    /// Detect a channel update between old and new L1 tips.
    ///
    /// Diffs the two channel *lineages*. `old_lineage` must be captured by the
    /// caller via [`Self::channel_lineage`] **before** this event's block is
    /// inserted, so the "before" side isn't contaminated by the just-added
    /// block; `new_lineage` is computed here, after the insert.
    /// - `adopted`: txs that entered the channel branch (first mined).
    /// - `orphaned`: txs that left it (replaced by a conflict). A bare un-mine
    ///   is a no-op — the link stays in the lineage via its held block.
    ///
    /// Returns `None` if no channel state change.
    #[must_use]
    pub fn detect_channel_update(
        &self,
        old_lineage: &[InscriptionInfo],
        new_tip: HeaderId,
    ) -> Option<ChannelUpdateInfo> {
        let new_channel_tip = self.channel_tip_at(new_tip);
        let new_lineage = self.channel_lineage(new_tip);

        let old_ids: HashSet<MsgId> = old_lineage.iter().map(|i| i.this_msg).collect();
        let new_ids: HashSet<MsgId> = new_lineage.iter().map(|i| i.this_msg).collect();

        let adopted = self.update_txs_from_infos(
            new_lineage
                .iter()
                .filter(|i| !old_ids.contains(&i.this_msg)),
        );

        let orphaned = self.update_txs_from_infos(
            old_lineage
                .iter()
                .filter(|i| !new_ids.contains(&i.this_msg)),
        );

        if orphaned.is_empty() && adopted.is_empty() {
            return None;
        }

        Some(ChannelUpdateInfo {
            orphaned,
            adopted,
            new_channel_tip,
        })
    }

    /// One update entry per tx: a multi-op custom tx contributes several
    /// lineage infos but is reported once, whole.
    fn update_txs_from_infos<'a>(
        &'a self,
        infos: impl Iterator<Item = &'a InscriptionInfo>,
    ) -> Vec<ChannelUpdateTx> {
        let mut seen: HashSet<TxHash> = HashSet::new();
        infos
            .filter(|info| seen.insert(info.tx_hash))
            .filter_map(|info| self.to_update_tx(info))
            .collect()
    }

    /// `None` for entries with no payload to apply (configs, config-only
    /// customs) — their effects reach consumers through the channel view.
    fn to_update_tx(&self, info: &InscriptionInfo) -> Option<ChannelUpdateTx> {
        if let Some(block_tx) = self
            .block_txs
            .values()
            .flatten()
            .find(|tx| tx.tx_hash() == Some(info.tx_hash))
        {
            return match block_tx {
                BlockChannelTx::AtomicWithdraw(a) => {
                    Some(ChannelUpdateTx::AtomicWithdraw(a.clone()))
                }
                BlockChannelTx::Inscription(_) => Some(ChannelUpdateTx::Inscription(info.clone())),
                BlockChannelTx::Config(_) => None,
                BlockChannelTx::Custom { tx, entries } => entries
                    .iter()
                    .any(|entry| !entry.payload.is_empty())
                    .then(|| ChannelUpdateTx::Custom(tx.clone())),
            };
        }
        // Not in any held block — the lineage bridged through a pending link.
        let withdraws = self
            .pending
            .get(&info.tx_hash)
            .and_then(|p| p.withdraws.clone());
        Some(withdraws.map_or_else(
            || ChannelUpdateTx::Inscription(info.clone()),
            |withdraws| {
                ChannelUpdateTx::AtomicWithdraw(AtomicWithdrawInfo {
                    tx_hash: info.tx_hash,
                    inscription: info.clone(),
                    withdraws,
                })
            },
        ))
    }

    /// The channel's inscription chain at an L1 tip: the mined inscriptions,
    /// extended forward through on-chain links we still hold whose position
    /// hasn't been taken by a competing inscription.
    ///
    /// Capture this at the *old* tip before inserting a new block; computing it
    /// afterwards would let the just-added block bridge into the "before" view.
    #[must_use]
    pub(crate) fn channel_lineage(&self, tip: HeaderId) -> Vec<InscriptionInfo> {
        let mut lineage = self.infos_on_branch(tip);
        let mut ids: HashSet<MsgId> = lineage.iter().map(|i| i.this_msg).collect();

        // Index every inscription we hold to form the channel lineage.
        let mut by_msg: HashMap<MsgId, InscriptionInfo> = HashMap::new();
        let mut children: HashMap<MsgId, HashSet<MsgId>> = HashMap::new();
        for info in self
            .block_txs
            .values()
            .flatten()
            .flat_map(BlockChannelTx::infos)
        {
            children
                .entry(info.parent_msg)
                .or_default()
                .insert(info.this_msg);
            by_msg.entry(info.this_msg).or_insert_with(|| info.clone());
        }

        // Walk forward from the mined tip, extending only where a single
        // un-replaced inscription chains off the current link; a contested
        // position (two competing children) ends the walk.
        let mut current = self.channel_tip_at(tip);
        while let Some(kids) = children.get(&current) {
            let mut candidates = kids.iter().filter(|id| !ids.contains(*id));
            let (Some(&next), None) = (candidates.next(), candidates.next()) else {
                break;
            };
            if let Some(info) = by_msg.get(&next) {
                lineage.push(info.clone());
            }
            ids.insert(next);
            current = next;
        }
        lineage
    }

    /// Collect ALL pending inscriptions reachable from `from_msg`.
    /// Uses the `pending_by_parent` index. Handles branching (multiple
    /// children per parent) by collecting all branches.
    /// Returns inscriptions in BFS order (parents before children).
    pub(crate) fn collect_pending_suffix(&self, from_msg: MsgId) -> Vec<InscriptionInfo> {
        let mut suffix = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(from_msg);

        while let Some(current) = queue.pop_front() {
            let Some(children) = self.pending_by_parent.get(&current) else {
                continue;
            };
            for child_hash in children {
                let Some(pending) = self.pending.get(child_hash) else {
                    continue;
                };
                suffix.push(InscriptionInfo {
                    tx_hash: pending.tx_hash,
                    parent_msg: pending.parent_msg,
                    this_msg: pending.this_msg,
                    payload: pending.payload.clone(),
                });
                queue.push_back(pending.this_msg);
            }
        }

        suffix
    }

    /// All tip-advancing entries on a branch back to LIB, oldest first.
    fn infos_on_branch(&self, tip: HeaderId) -> Vec<InscriptionInfo> {
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
                self.block_txs.get(&block_id).map_or_else(Vec::new, |txs| {
                    txs.iter()
                        .flat_map(BlockChannelTx::infos)
                        .cloned()
                        .collect()
                })
            })
            .collect()
    }

    #[must_use]
    pub fn collect_update_txs_on_branch(&self, tip: HeaderId) -> Vec<ChannelUpdateTx> {
        self.update_txs_from_infos(self.infos_on_branch(tip).iter())
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{
        MantleTx, Op::ChannelInscribe, Transaction as _, ops::channel::inscribe::InscriptionOp,
    };
    use lb_key_management_system_service::keys::Ed25519PublicKey;

    use super::*;

    fn header_id(n: u8) -> HeaderId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        HeaderId::from(bytes)
    }

    fn make_dummy_tx(data: u8) -> SignedMantleTx {
        let mantle_tx = MantleTx(
            [ChannelInscribe(InscriptionOp {
                channel_id: [0u8; 32].into(),
                inscription: [data].into(),
                parent: [0u8; 32].into(),
                signer: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            })]
            .into(),
        );
        SignedMantleTx {
            ops_proofs: vec![],
            mantle_tx,
        }
    }

    #[test]
    fn submit_and_query_pending() {
        let genesis = header_id(0);
        let mut state = TxState::new(genesis, MsgId::root());
        let tx = make_dummy_tx(1);

        state.submit_other(tx, ChannelId::from([0u8; 32]));
        assert_eq!(state.unfinalized_count(), 1);
    }

    #[test]
    fn block_includes_tx() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit_other(tx, ChannelId::from([0u8; 32]));

        // Process block containing our tx, lib stays at genesis
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);

        // Tx is still pending (not finalized yet, lib hasn't advanced)
        assert_eq!(state.unfinalized_count(), 1);

        // But pending_txs at b1 excludes it (it's in the safe set)
        assert!(state.pending_txs(b1).is_empty());
    }

    #[test]
    fn lib_advance_finalizes() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit_other(tx, ChannelId::from([0u8; 32]));

        // b1 with our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);
        assert_eq!(state.unfinalized_count(), 1);

        // b2, lib advances to b1 — process_block does not remove from
        // pending (that's done by backfill ground truth)
        state.process_block(b2, b1, b1, vec![], vec![]);
        assert_eq!(
            state.unfinalized_count(),
            1,
            "tx still in pending until backfill confirms"
        );

        // Simulate backfill confirming the tx
        assert!(state.remove_pending(&hash).is_some());
        assert_eq!(state.unfinalized_count(), 0);
    }

    #[test]
    fn pending_txs_excludes_safe() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx1 = make_dummy_tx(1);
        let tx2 = make_dummy_tx(2);
        let hash1 = tx1.mantle_tx.hash();
        let hash2 = tx2.mantle_tx.hash();

        state.submit_other(tx1, ChannelId::from([0u8; 32]));
        state.submit_other(tx2, ChannelId::from([0u8; 32]));

        // b1 contains only tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![]);

        // pending_txs at b1 should only return tx2
        let pending: Vec<_> = state.pending_txs(b1).into_iter().map(|(h, _)| h).collect();
        assert_eq!(pending.len(), 1);
        assert!(pending.contains(&hash2));
    }

    #[test]
    fn reorg_changes_pending_status() {
        // G -> b1 (has tx)
        //   -> b2 (no tx)
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit_other(tx, ChannelId::from([0u8; 32]));

        // b1 has our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);

        // At b1 tip, tx is in safe set (not in pending_txs)
        assert!(state.pending_txs(b1).is_empty());

        // b2 forks from genesis, no tx
        state.process_block(b2, genesis, genesis, vec![], vec![]);

        // At b2 tip, tx is back in pending_txs (different branch)
        assert!(state.pending_txs(b2).iter().any(|(h, _)| *h == hash));
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

        let mut state = TxState::new(genesis, MsgId::root());

        // Build main chain up to a1
        state.process_block(a1, genesis, genesis, vec![], vec![]);

        // Build fork from a1 (before lib advances past a1)
        state.process_block(b1, a1, genesis, vec![], vec![]);
        state.process_block(b2, b1, genesis, vec![], vec![]);

        // Verify fork blocks exist before lib advances
        assert!(state.block_states.contains_key(&b1));
        assert!(state.block_states.contains_key(&b2));

        // Continue main chain, lib advances to a3
        state.process_block(a2, a1, genesis, vec![], vec![]);
        state.process_block(a3, a2, a3, vec![], vec![]); // lib advances to a3

        // After lib advances to a3:
        // - genesis, a1, a2 should be pruned (ancestors up to and including old lib)
        // - b1, b2 should be GC'd (orphans - their ancestor a1 was pruned)
        // - a3 (new lib) should exist

        assert!(
            !state.block_states.contains_key(&genesis),
            "genesis (old lib) should be pruned"
        );
        assert!(!state.block_states.contains_key(&a1), "a1 should be pruned");
        assert!(!state.block_states.contains_key(&a2), "a2 should be pruned");
        assert!(
            !state.block_states.contains_key(&b1),
            "orphan b1 should be pruned"
        );
        assert!(
            !state.block_states.contains_key(&b2),
            "orphan b2 should be pruned"
        );

        assert!(state.block_states.contains_key(&a3), "lib should exist");

        // Continue and verify pruning continues working
        state.process_block(a4, a3, a3, vec![], vec![]);
        state.process_block(a5, a4, a5, vec![], vec![]); // lib advances to a5
        state.process_block(a6, a5, a5, vec![], vec![]);

        assert!(
            !state.block_states.contains_key(&a3),
            "old lib should be pruned"
        );
        assert!(!state.block_states.contains_key(&a4), "a4 should be pruned");
        assert!(state.block_states.contains_key(&a5), "new lib should exist");
        assert!(state.block_states.contains_key(&a6), "tip should exist");
    }

    fn msg_id(n: u8) -> MsgId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        MsgId::from(bytes)
    }

    /// Submit a fake pending inscription with lineage metadata.
    fn submit_fake_inscription(
        state: &mut TxState,
        data: u8,
        parent_msg: MsgId,
        this_msg: MsgId,
    ) -> TxHash {
        let tx = make_dummy_tx(data);
        let hash = tx.mantle_tx.hash();
        state.submit_inscription(tx, parent_msg, this_msg, [data].into());
        hash
    }

    #[test]
    fn extension_with_competing_inscription_does_not_orphan_local_pending() {
        // Scenario: local pending b1→b2→b3 from root.
        // Competing c1 lands on chain consuming root as parent.
        // This is an extension — no blocks removed from canonical.
        // Under the block-delta semantics, `orphaned` stays empty; the
        // local pending b1→b2→b3 were never on canonical so they are not
        // reported. They remain in `self.pending` (invalid on current tip,
        // eligible for cleanup when their branch falls below LIB).
        let genesis = header_id(0);
        let block1 = header_id(1);
        let block2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let b2_msg = msg_id(11);
        let b3_msg = msg_id(12);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 2, b1_msg, b2_msg);
        submit_fake_inscription(&mut state, 3, b2_msg, b3_msg);
        assert_eq!(state.pending.len(), 3);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        // Capture the old lineage before inserting block2, mirroring the real
        // caller; computing it after would let c1 bridge into the "before" view.
        let old_lineage = state.channel_lineage(block1);

        let c1_msg = msg_id(20);
        let c1_tx = make_dummy_tx(99);
        let c1_tx_hash = c1_tx.mantle_tx.hash();
        let c1_inscription = InscriptionInfo {
            tx_hash: c1_tx_hash,
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: [99].into(),
        };
        // Mirror the observed inscription into pending before the safe-set
        // build, as `handle_block_event` does — the pending set reflects the
        // channel view, so c1 is retried too if it later reorgs out.
        state.observe_channel_inscription(c1_tx, MsgId::root(), c1_msg, [99].into(), None);
        state.process_block(
            block2,
            block1,
            genesis,
            vec![c1_tx_hash],
            vec![BlockChannelTx::Inscription(c1_inscription)],
        );

        let update = state
            .detect_channel_update(&old_lineage, block2)
            .expect("should detect channel update");

        assert!(update.orphaned.is_empty(), "extension never orphans");
        assert_eq!(update.adopted.len(), 1);
        assert_eq!(update.adopted[0].inscription().unwrap().this_msg, c1_msg);
        // Local pending is still tracked, and the observed network entry
        // joined it (already `posted`, excluded from re-posting while its
        // block is on-branch via the safe set).
        assert_eq!(state.pending.len(), 4);
        assert!(state.is_tracked(&c1_tx_hash));
        assert!(
            state
                .pending_txs(block2)
                .iter()
                .all(|(hash, _)| *hash != c1_tx_hash),
            "on-branch observed entry must not be re-posted"
        );
    }

    #[test]
    fn extension_with_competing_inscription_does_not_orphan_multiple_pending_roots() {
        // Two independent pending inscriptions both target root as parent.
        // Competing c1 lands consuming root. Neither is reported as
        // orphaned under the block-delta semantics; both remain in pending.
        let genesis = header_id(0);
        let block1 = header_id(1);
        let block2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let d1_msg = msg_id(30);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 4, MsgId::root(), d1_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        // Capture the old lineage before inserting block2, mirroring the real
        // caller; computing it after would let c1 bridge into the "before" view.
        let old_lineage = state.channel_lineage(block1);

        let c1_msg = msg_id(20);
        let c1_inscription = InscriptionInfo {
            tx_hash: make_dummy_tx(99).mantle_tx.hash(),
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: [99].into(),
        };
        state.process_block(
            block2,
            block1,
            genesis,
            vec![],
            vec![BlockChannelTx::Inscription(c1_inscription)],
        );

        let update = state.detect_channel_update(&old_lineage, block2).unwrap();
        assert!(update.orphaned.is_empty());
        assert_eq!(update.adopted.len(), 1);
        assert_eq!(update.adopted[0].inscription().unwrap().this_msg, c1_msg);
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn fragmented_pending_publish_falls_back_to_canonical() {
        // Two independent pending inscriptions both chain from root.
        // This is ambiguous (2 children of root), so publish_parent
        // should fall back to canonical tip (root), not pick one
        // arbitrarily.
        let genesis = header_id(0);
        let block1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let d1_msg = msg_id(30);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 4, MsgId::root(), d1_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        // Ambiguous: two children of root → falls back to canonical tip
        assert_eq!(state.publish_parent(block1), MsgId::root());
    }

    #[test]
    fn linear_pending_suffix_extends_from_tail() {
        // Linear pending chain: root → b1 → b2.
        // publish_parent should return b2 (the tail).
        let genesis = header_id(0);
        let block1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let b2_msg = msg_id(11);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 2, b1_msg, b2_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        assert_eq!(state.publish_parent(block1), b2_msg);
    }

    #[test]
    fn stale_pending_tail_not_reused_for_publish() {
        // Local pending b1 from root. c1 lands consuming root.
        // publish_parent should return c1 (canonical tip), not b1.
        let genesis = header_id(0);
        let block1 = header_id(1);
        let block2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        // c1 lands, consuming root
        let c1_msg = msg_id(20);
        let c1_inscription = InscriptionInfo {
            tx_hash: make_dummy_tx(99).mantle_tx.hash(),
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: [99].into(),
        };
        state.process_block(
            block2,
            block1,
            genesis,
            vec![],
            vec![BlockChannelTx::Inscription(c1_inscription)],
        );

        // b1 is stale — publish_parent should return canonical tip (c1)
        assert_eq!(state.publish_parent(block2), c1_msg);
    }

    #[test]
    fn multi_block_lib_advance_finalizes_intermediate() {
        // When LIB advances multiple blocks at once, all intermediate txs must finalize
        // genesis <- b1 (tx1) <- b2 (tx2) <- b3
        //                                     ^
        //                                    LIB jumps here
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let b3 = header_id(3);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx1 = make_dummy_tx(1);
        let tx2 = make_dummy_tx(2);
        let hash1 = tx1.mantle_tx.hash();
        let hash2 = tx2.mantle_tx.hash();

        state.submit_other(tx1, ChannelId::from([0u8; 32]));
        state.submit_other(tx2, ChannelId::from([0u8; 32]));

        // b1 has tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![]);
        // b2 has tx2
        state.process_block(b2, b1, genesis, vec![hash2], vec![]);
        // b3, lib jumps from genesis to b2 (skipping b1)
        state.process_block(b3, b2, b2, vec![], vec![]);
        assert_eq!(
            state.unfinalized_count(),
            2,
            "txs still pending until backfill"
        );

        // Simulate backfill confirming both txs
        assert!(state.remove_pending(&hash1).is_some());
        assert!(state.remove_pending(&hash2).is_some());
        assert_eq!(state.unfinalized_count(), 0);
    }
}
