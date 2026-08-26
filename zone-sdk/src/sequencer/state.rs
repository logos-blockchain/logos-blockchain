use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use lb_core::{
    header::HeaderId,
    mantle::{
        SignedMantleTx,
        ops::{
            Op,
            channel::{ChannelId, MsgId, inscribe::Inscription},
        },
        traits::Hashable as _,
        transactions::{hash::TxHash, mantle_tx::MantleTx as _, states::Unverified},
    },
};
use rpds::HashTrieSetSync;

use super::{
    channel_wallet::{ChannelWallet, NoteOp},
    types::{
        AtomicWithdrawInfo, ChannelNote, ChannelUpdateTx, ChannelWalletView, InscriptionInfo,
        PendingTx, TxSource, WithdrawInfo,
    },
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

/// `first_parent`/`last_msg` anchor the tx in the message lineage via its
/// inscriptions; `config_parent`/`last_config` anchor it in the config
/// lineage via its configs. A tx anchored in neither is always mineable and
/// never shed.
#[derive(Debug, Clone)]
struct PendingOtherTx {
    signed_tx: SignedMantleTx<Unverified>,
    first_parent: Option<MsgId>,
    last_msg: Option<MsgId>,
    config_parent: Option<MsgId>,
    last_config: Option<MsgId>,
    /// Submission order, for checkpoint serialization.
    seq: u64,
}

fn opaque_lineage(
    tx: &SignedMantleTx<Unverified>,
    channel_id: ChannelId,
) -> (Option<MsgId>, Option<MsgId>, Option<MsgId>, Option<MsgId>) {
    let mut first_parent = None;
    let mut last_msg = None;
    let mut config_parent = None;
    let mut last_config = None;
    for op in tx.mantle_tx().ops() {
        match op {
            Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                if last_msg.is_none() {
                    first_parent = Some(inscribe.parent);
                }
                last_msg = Some(inscribe.id());
            }
            Op::ChannelConfig(config) if config.channel == channel_id => {
                if last_config.is_none() {
                    config_parent = Some(config.parent);
                }
                last_config = Some(config.id());
            }
            _ => {}
        }
    }
    (first_parent, last_msg, config_parent, last_config)
}

/// Local pending inscription with lineage metadata.
///
/// `withdraws == None` is a plain inscription; `Some(_)` is an atomic
/// inscription+withdraw bundle. The bundle nature lets us surface the right
/// [`PendingTx`] variant on finalize/adopt and re-prepare on orphan.
#[derive(Debug, Clone)]
pub struct PendingInscription {
    pub tx_hash: TxHash,
    pub signed_tx: SignedMantleTx<Unverified>,
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
    /// Monotonic submission counter for [`Self::pending_other`] entries.
    next_other_seq: u64,
    /// Lineage-parent of the entry behind [`Self::finalized_msg`] — the
    /// finalized entry is matched as a `(this_msg, parent_msg)` pair. `None`
    /// when the finalized entry is unknown (fresh state or checkpoint
    /// restore); the finalized-prefix search then matches nothing (see
    /// [`Self::finalized_prefix_ids`]).
    finalized_parent_msg: Option<MsgId>,
    /// The config-lineage tip at LIB — the newest config finalized so far, or
    /// [`MsgId::root`] when none has finalized (or is unknown after a
    /// checkpoint restore). Seeds the landable set in
    /// [`Self::shed_stale_pending_configs`] so a pending config chaining on the
    /// finalized config tip is not falsely orphaned.
    finalized_config: MsgId,
    /// Config tip last seen by the config-driven inscription shed; a change
    /// means a config landed (or the branch's config lineage diverged).
    observed_config_tip: MsgId,
    /// The channel's note set: finalized base + per-block overlay.
    wallet: ChannelWallet,
}

/// A channel-touching tx's tip-advancing content, classified once at block
/// scan and stored per block.
#[derive(Debug, Clone)]
pub enum BlockChannelTx {
    /// `publish` shape: a single inscription.
    Inscription(InscriptionInfo),
    /// `publish_atomic_withdraw` shape: an inscription + its withdraws.
    AtomicWithdraw(AtomicWithdrawInfo),
    /// A pure `channel_config` tx: a single config on the config lineage
    /// (`this_msg` = config id, `parent_msg` = config parent), which does not
    /// advance the message tip.
    Config(InscriptionInfo),
    /// A shape the SDK cannot produce (bundled deposits, multi-inscribe,
    /// custom-built txs). Kept whole — updates hand the tx back to the
    /// caller's own recovery logic — along with its tip-advancing entries
    /// in op order. `config_entries` holds any `ChannelConfig` ops it carries,
    /// which sit on the separate config lineage and never advance the message
    /// tip.
    Custom {
        tx: SignedMantleTx<Unverified>,
        entries: Vec<InscriptionInfo>,
        config_entries: Vec<InscriptionInfo>,
    },
}

impl BlockChannelTx {
    /// The tip-advancing entries of this tx, in op order.
    pub fn infos(&self) -> &[InscriptionInfo] {
        match self {
            Self::Inscription(i) => std::slice::from_ref(i),
            Self::AtomicWithdraw(a) => std::slice::from_ref(&a.inscription),
            Self::Config(_) => &[],
            Self::Custom { entries, .. } => entries,
        }
    }

    /// The `ChannelConfig` entries this tx carries, in op order. These are on
    /// the config lineage (`this_msg` = config id, `parent_msg` = config
    /// parent) and do not advance the message tip. The clean
    /// `Inscription`/`AtomicWithdraw` shapes carry none; a pure config is a
    /// [`Self::Config`]; mixed/unknown configs ride in [`Self::Custom`].
    pub fn config_entries(&self) -> &[InscriptionInfo] {
        match self {
            Self::Inscription(_) | Self::AtomicWithdraw(_) => &[],
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
            finalized_parent_msg: None,
            finalized_config: MsgId::root(),
            observed_config_tip: MsgId::root(),
            wallet: ChannelWallet::default(),
            next_other_seq: 0,
        }
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

    /// Submit an inscription tx for tracking with lineage metadata. Use
    /// [`Self::submit_atomic_withdraw`] for inscription+withdraw bundles.
    pub fn submit_inscription(
        &mut self,
        signed_tx: SignedMantleTx<Unverified>,
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
        signed_tx: SignedMantleTx<Unverified>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        withdraws: Vec<WithdrawInfo>,
    ) {
        self.insert_pending(signed_tx, parent_msg, this_msg, payload, Some(withdraws));
    }

    fn insert_pending(
        &mut self,
        signed_tx: SignedMantleTx<Unverified>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        withdraws: Option<Vec<WithdrawInfo>>,
    ) {
        let tx_hash = signed_tx.mantle_tx().hash();
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
        signed_tx: SignedMantleTx<Unverified>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        withdraws: Option<Vec<WithdrawInfo>>,
    ) {
        let tx_hash = signed_tx.mantle_tx().hash();
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

    /// Returns the channel tip the tx leaves behind once mined (its last
    /// tip-advancing op), or `None` when it carries none for this channel.
    pub fn submit_other(
        &mut self,
        signed_tx: SignedMantleTx<Unverified>,
        channel_id: ChannelId,
    ) -> Option<MsgId> {
        let tx_hash = signed_tx.mantle_tx().hash();
        let (first_parent, last_msg, config_parent, last_config) =
            opaque_lineage(&signed_tx, channel_id);
        self.track_local_tx(tx_hash);
        let seq = self.next_other_seq;
        self.next_other_seq += 1;
        self.pending_other.insert(
            tx_hash,
            PendingOtherTx {
                signed_tx,
                first_parent,
                last_msg,
                config_parent,
                last_config,
                seq,
            },
        );
        last_msg
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
        note_ops: Vec<NoteOp>,
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
        self.wallet.store_overlay(block_id, note_ops);

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

            // Prune ancestors of new lib (but not lib itself)
            let mut prune_cursor = self.parent_map.get(&lib).copied();
            while let Some(b) = prune_cursor {
                self.block_states.remove(&b);
                self.block_txs.remove(&b);
                self.wallet.prune_block(&b);
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
                self.wallet.prune_block(&orphan);
                self.parent_map.remove(&orphan);
            }
        }
    }

    /// Pending txs eligible for resubmission: not yet safe at tip AND
    /// part of the local suffix reachable from canonical channel tip.
    ///
    /// Returned in parent-before-child order so the node's mempool sees the
    /// parent before any child: inscriptions via BFS from channel tip
    /// (`pending_by_parent`), opaque txs by submission order (`seq`) — a
    /// locally chained bundle can only be built after the bundle that
    /// establishes its parent tip, so submission order is dependency order.
    pub fn pending_txs(&self, tip: HeaderId) -> Vec<(TxHash, SignedMantleTx<Unverified>)> {
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
        let mut others: Vec<_> = self
            .pending_other
            .iter()
            .filter(|(hash, _)| !safe.contains(hash))
            .collect();
        others.sort_unstable_by_key(|(_, entry)| entry.seq);
        inscriptions
            .chain(
                others
                    .into_iter()
                    .map(|(hash, entry)| (*hash, entry.signed_tx.clone())),
            )
            .collect()
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
    /// a fresh `parent_msg`; plain inscriptions are returned
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
        self.drain_pending_in_lineage_order(&eligible)
    }

    /// Remove the given pending inscriptions/bundles in parent-first lineage
    /// order, returning them as [`PendingTx`] for orphan reporting. Shared by
    /// the off-branch and config-driven sheds.
    fn drain_pending_in_lineage_order(&mut self, eligible: &HashSet<TxHash>) -> Vec<PendingTx> {
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

    /// On a config-tip change, shed the pending entries **not on this branch's
    /// tip** (not in the safe set) — the not-yet-mined tail a config may have
    /// invalidated. Mined/on-branch entries are excluded: a config never
    /// invalidates a landed inscription, so orphaning one would re-post an
    /// on-chain original as a duplicate. The caller resets the chaining pointer
    /// to the message tip so re-posts land as a competing branch. Unchanged
    /// config tip → empty.
    pub fn shed_pending_inscriptions_on_config(&mut self, tip: HeaderId) -> Vec<PendingTx> {
        let config_tip = self.config_tip_at(tip);
        if config_tip == self.observed_config_tip {
            return Vec::new();
        }
        self.observed_config_tip = config_tip;

        let safe: HashSet<TxHash> = self
            .block_states
            .get(&tip)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let eligible: HashSet<TxHash> = self
            .pending
            .keys()
            .filter(|h| !safe.contains(h))
            .copied()
            .collect();
        self.drain_pending_in_lineage_order(&eligible)
    }

    /// Shed pending opaque txs whose first inscription's parent slot was
    /// consumed by a conflicting entry: removed from retry and returned
    /// whole for orphan reporting.
    pub fn shed_off_branch_pending_other(
        &mut self,
        tip: HeaderId,
    ) -> Vec<SignedMantleTx<Unverified>> {
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

    /// Shed pending config-carrying txs whose config parent can no longer
    /// reach the mined config tip: removed from retry and returned whole for
    /// orphan reporting.
    pub fn shed_stale_pending_configs(&mut self, tip: HeaderId) -> Vec<SignedMantleTx<Unverified>> {
        if self.pending_other.is_empty() {
            return Vec::new();
        }
        // Seed with the config tip we have actually processed on this branch —
        // not the node's config tip, which can race ahead of the blocks we have
        // processed and falsely orphan a pending config that merely extends our
        // local tip (its block just hasn't arrived yet).
        let mut landable: HashSet<MsgId> = HashSet::new();
        landable.insert(self.config_tip_at(tip));
        // A viable entry makes its own last config landable, so entries
        // chained on it are kept too.
        loop {
            let mut changed = false;
            for entry in self.pending_other.values() {
                let viable = entry
                    .config_parent
                    .is_none_or(|parent| landable.contains(&parent));
                if viable && let Some(last_config) = entry.last_config {
                    changed |= landable.insert(last_config);
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
                // An entry whose own config already landed on this branch (its
                // `last_config` is in `landable`) stays — even if its block is
                // not yet in the safe set.
                let landed = entry
                    .last_config
                    .is_some_and(|last_config| landable.contains(&last_config));
                !safe.contains(*hash)
                    && !landed
                    && entry
                        .config_parent
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
    pub fn all_pending_txs(&self) -> Vec<(TxHash, SignedMantleTx<Unverified>)> {
        let inscriptions = self
            .pending
            .iter()
            .map(|(hash, p)| (*hash, p.signed_tx.clone()));
        let mut others: Vec<_> = self.pending_other.iter().collect();
        others.sort_unstable_by_key(|(_, entry)| entry.seq);
        inscriptions
            .chain(
                others
                    .into_iter()
                    .map(|(hash, entry)| (*hash, entry.signed_tx.clone())),
            )
            .collect()
    }

    /// Remove a pending inscription and return its signed tx.
    pub fn remove_pending(&mut self, tx_hash: &TxHash) -> Option<SignedMantleTx<Unverified>> {
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

        // Bounded by the pending population: a longer walk would mean a
        // cycle in (possibly inconsistent) pending lineage data.
        for _ in 0..=(self.pending.len() + self.pending_other.len()) {
            let Some(next) = self.single_pending_child(current) else {
                return found_any.then_some(current);
            };
            current = next;
            found_any = true;
        }
        found_any.then_some(current)
    }

    /// The unique pending link chaining off `current`, considering both plain
    /// pending inscriptions and opaque pending txs (`submit_signed_tx`
    /// bundles), which enter the chain at their first inscription's parent
    /// and leave it at their last tip-advancing op. A contested position
    /// (multiple candidates) yields `None` so the caller stops before it.
    fn single_pending_child(&self, current: MsgId) -> Option<MsgId> {
        let mut candidate: Option<MsgId> = None;
        let mut push = |msg: MsgId| -> bool {
            if candidate.is_some() && candidate != Some(msg) {
                return false;
            }
            candidate = Some(msg);
            true
        };

        if let Some(children) = self.pending_by_parent.get(&current) {
            for tx_hash in children {
                if let Some(pending) = self.pending.get(tx_hash)
                    && !push(pending.this_msg)
                {
                    return None;
                }
            }
        }
        for other in self.pending_other.values() {
            if other.first_parent == Some(current)
                && let Some(last_msg) = other.last_msg
                && last_msg != current
                && !push(last_msg)
            {
                return None;
            }
        }
        candidate
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
            if let Some(txs) = self.block_txs.get(&current)
                && let Some(entry) = txs.iter().rev().find_map(BlockChannelTx::tip_entry)
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
            if let Some(txs) = self.block_txs.get(&current)
                && let Some(entry) = txs.iter().rev().find_map(|tx| tx.config_entries().last())
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
    /// Content at or below the finalized boundary is excluded from both
    /// sides: it is immutable on every branch and surfaces via `finalized`.
    ///
    /// Returns `None` only when the channel did not change at all. A change
    /// made purely of non-reportable entries yields `Some` with empty
    /// `adopted`/`orphaned` — the tip still moved, and callers must run
    /// their shed pass on every reported update.
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

        // Each lineage stops at the LIB of its capture time, so a LIB
        // advance between the captures shifts the diff's lower boundary.
        // Mask the finalized prefix on both sides so the shifted floor
        // doesn't read as adopted/orphaned content.
        let mut finalized = self.finalized_prefix_ids(old_lineage);
        finalized.extend(self.finalized_prefix_ids(&new_lineage));

        let adopted_infos: Vec<&InscriptionInfo> = new_lineage
            .iter()
            .filter(|i| !old_ids.contains(&i.this_msg) && !finalized.contains(&i.this_msg))
            .collect();

        let orphaned_infos: Vec<&InscriptionInfo> = old_lineage
            .iter()
            .filter(|i| !new_ids.contains(&i.this_msg) && !finalized.contains(&i.this_msg))
            .collect();

        // Decide on the raw diff, before reportability filtering: a change
        // of non-reportable entries still moves the tip, and callers must
        // run their shed pass on it.
        if adopted_infos.is_empty() && orphaned_infos.is_empty() {
            return None;
        }

        let adopted = self.update_txs_from_infos(adopted_infos.into_iter());
        let orphaned = self.update_txs_from_infos(orphaned_infos.into_iter());

        Some(ChannelUpdateInfo {
            orphaned,
            adopted,
            new_channel_tip,
        })
    }

    /// Msg-ids of `lineage`'s prefix up to and including the finalized entry;
    /// empty when the finalized boundary lies below the lineage's start.
    /// The entry is matched as a `(this_msg, parent_msg)` pair, last
    /// occurrence taken.
    ///
    /// An unknown parent (fresh state or checkpoint restore) matches
    /// nothing: every boundary move records the parent, so until one happens
    /// the boundary entry sits at-or-below the LIB and cannot appear in a
    /// lineage.
    fn finalized_prefix_ids(&self, lineage: &[InscriptionInfo]) -> HashSet<MsgId> {
        lineage
            .iter()
            .rposition(|i| {
                i.this_msg == self.finalized_msg
                    && self
                        .finalized_parent_msg
                        .is_some_and(|parent| i.parent_msg == parent)
            })
            .map_or_else(HashSet::new, |pos| {
                lineage[..=pos].iter().map(|i| i.this_msg).collect()
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

    /// `None` for entries with no payload to apply — their effects reach
    /// consumers through the channel view.
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
                // A pure config carries no message-lineage entry to report.
                BlockChannelTx::Config(_) => None,
                BlockChannelTx::Custom { tx, entries, .. } => entries
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

    /// Apply channel-note ops from finalized blocks to the wallet base set.
    pub fn apply_finalized_note_ops(&mut self, ops: Vec<NoteOp>) {
        self.wallet.apply_finalized(ops);
    }

    /// The channel's note set at `tip` (or the finalized base only when no
    /// tip is known yet). The overlay walk excludes the LIB block: blocks at
    /// and below LIB reach the base via the finalized-backfill path.
    #[must_use]
    pub fn channel_wallet_view(&self, tip: Option<HeaderId>) -> ChannelWalletView {
        let mut blocks = Vec::new();
        if let Some(tip) = tip {
            let mut current = tip;
            while current != self.current_lib {
                blocks.push(current);
                match self.parent_map.get(&current) {
                    Some(&parent) => current = parent,
                    None => break,
                }
            }
            blocks.reverse();
        }
        self.wallet.view(blocks.iter())
    }

    /// Export the finalized channel-note base for checkpointing.
    #[must_use]
    pub fn channel_notes_base(&self) -> Vec<ChannelNote> {
        self.wallet.export_base()
    }

    /// Restore the finalized channel-note base from a checkpoint.
    pub fn restore_channel_notes(&mut self, notes: Vec<ChannelNote>) {
        self.wallet.restore_base(notes);
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
        Op::ChannelInscribe, RawMantleTx, ops::channel::inscribe::InscriptionOp,
        transactions::OpsProofs,
    };
    use lb_key_management_system_service::keys::Ed25519PublicKey;

    use super::*;
    use crate::test_support::header_id;

    fn make_dummy_tx(data: u8) -> SignedMantleTx<Unverified> {
        let mantle_tx = RawMantleTx(
            [ChannelInscribe(InscriptionOp {
                channel_id: [0u8; 32].into(),
                inscription: [data].into(),
                parent: [0u8; 32].into(),
                signer: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            })]
            .into(),
        );
        SignedMantleTx::new(mantle_tx, OpsProofs::empty())
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
        let hash = tx.mantle_tx().hash();
        state.submit_other(tx, ChannelId::from([0u8; 32]));

        // Process block containing our tx, lib stays at genesis
        state.process_block(b1, genesis, genesis, vec![hash], vec![], Vec::new());

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
        let hash = tx.mantle_tx().hash();
        state.submit_other(tx, ChannelId::from([0u8; 32]));

        // b1 with our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![], Vec::new());
        assert_eq!(state.unfinalized_count(), 1);

        // b2, lib advances to b1 — process_block does not remove from
        // pending (that's done by backfill ground truth)
        state.process_block(b2, b1, b1, vec![], vec![], Vec::new());
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
        let hash1 = tx1.mantle_tx().hash();
        let hash2 = tx2.mantle_tx().hash();

        state.submit_other(tx1, ChannelId::from([0u8; 32]));
        state.submit_other(tx2, ChannelId::from([0u8; 32]));

        // b1 contains only tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![], Vec::new());

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
        let hash = tx.mantle_tx().hash();
        state.submit_other(tx, ChannelId::from([0u8; 32]));

        // b1 has our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![], Vec::new());

        // At b1 tip, tx is in safe set (not in pending_txs)
        assert!(state.pending_txs(b1).is_empty());

        // b2 forks from genesis, no tx
        state.process_block(b2, genesis, genesis, vec![], vec![], Vec::new());

        // At b2 tip, tx is back in pending_txs (different branch)
        assert!(state.pending_txs(b2).iter().any(|(h, _)| *h == hash));
    }

    fn wallet_note(seed: u64, value: u64) -> NoteOp {
        NoteOp::Add(ChannelNote {
            note_id: lb_core::mantle::ledger::NoteId::from(lb_groth16::Fr::from(seed)),
            value,
            pk: lb_groth16::Fr::from(seed).into(),
            slot: lb_common_http_client::Slot::from(1),
        })
    }

    fn wallet_note_id(seed: u64) -> lb_core::mantle::ledger::NoteId {
        lb_core::mantle::ledger::NoteId::from(lb_groth16::Fr::from(seed))
    }

    #[test]
    fn wallet_view_follows_branch() {
        // G <- a1 (adds n1)
        //   <- b1 (adds n2)
        let genesis = header_id(0);
        let a1 = header_id(1);
        let b1 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        state.process_block(
            a1,
            genesis,
            genesis,
            vec![],
            vec![],
            vec![wallet_note(1, 10)],
        );
        state.process_block(
            b1,
            genesis,
            genesis,
            vec![],
            vec![],
            vec![wallet_note(2, 20)],
        );

        let at_a = state.channel_wallet_view(Some(a1));
        assert_eq!(at_a.unfinalized.len(), 1);
        assert_eq!(at_a.unfinalized[0].note_id, wallet_note_id(1));

        let at_b = state.channel_wallet_view(Some(b1));
        assert_eq!(at_b.unfinalized.len(), 1);
        assert_eq!(at_b.unfinalized[0].note_id, wallet_note_id(2));
    }

    #[test]
    fn wallet_lib_advance_excludes_folded_overlay() {
        // G <- a1 (adds n1) <- a2; LIB advances to a1. The finalized-backfill
        // path applies a1's ops to the base; the branch walk from a2 must
        // exclude a1's overlay entry so the note is not double-counted.
        let genesis = header_id(0);
        let a1 = header_id(1);
        let a2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        state.process_block(
            a1,
            genesis,
            genesis,
            vec![],
            vec![],
            vec![wallet_note(1, 10)],
        );
        // What `fetch_and_process_blocks` does when a1's range finalizes:
        state.apply_finalized_note_ops(vec![wallet_note(1, 10)]);
        state.process_block(a2, a1, a1, vec![], vec![], Vec::new());

        let view = state.channel_wallet_view(Some(a2));
        assert_eq!(view.finalized.len(), 1);
        assert_eq!(view.finalized[0].note_id, wallet_note_id(1));
        assert!(
            view.unfinalized.is_empty(),
            "a1's overlay entry must not double-count the finalized note"
        );
    }

    /// Build an `[inscribe(parent), config]` bundle tx for the zero channel.
    fn bundle_tx(parent: MsgId, data: u8) -> (SignedMantleTx<Unverified>, MsgId, MsgId) {
        use lb_core::mantle::{
            channel::{SlotTimeframe, SlotTimeout},
            ops::channel::config::{ChannelConfigOp, Keys},
        };
        let inscribe = InscriptionOp {
            channel_id: [0u8; 32].into(),
            inscription: [data].into(),
            parent,
            signer: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
        };
        let config = ChannelConfigOp {
            channel: [0u8; 32].into(),
            parent: MsgId::root(),
            keys: Keys::try_from(vec![Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap()]).unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let inscribe_msg = inscribe.id();
        let config_msg = config.id();
        let tx = SignedMantleTx::new(
            RawMantleTx([ChannelInscribe(inscribe), Op::ChannelConfig(config)].into()),
            OpsProofs::empty(),
        );
        (tx, inscribe_msg, config_msg)
    }

    /// A pending `submit_signed_tx` bundle must participate in publish-parent
    /// chaining: the next publish chains off the bundle's last inscription,
    /// not the pre-bundle tip — otherwise the two txs race for the same
    /// channel position and one is permanently invalidated. The config moves
    /// only the config lineage.
    #[test]
    fn publish_parent_chains_through_pending_bundle() {
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(tip, genesis, genesis, vec![], vec![], Vec::new());

        let (bundle, inscribe_msg, _config_msg) = bundle_tx(MsgId::root(), 1);
        let derived = state.submit_other(bundle, channel_id);
        assert_eq!(derived, Some(inscribe_msg), "bundle tip is its inscription");

        assert_eq!(
            state.publish_parent(tip),
            inscribe_msg,
            "next publish must chain after the pending bundle"
        );
    }

    #[test]
    fn wallet_prunes_orphaned_branch_entries() {
        // G <- a1 <- a2 (lib advances to a1); b1 forks from G with a note.
        // After the lib advance b1 is pruned, so its note is unreachable
        // even if a stale tip were queried.
        let genesis = header_id(0);
        let a1 = header_id(1);
        let b1 = header_id(2);
        let a2 = header_id(3);
        let mut state = TxState::new(genesis, MsgId::root());

        state.process_block(a1, genesis, genesis, vec![], vec![], Vec::new());
        state.process_block(
            b1,
            genesis,
            genesis,
            vec![],
            vec![],
            vec![wallet_note(2, 20)],
        );
        state.process_block(a2, a1, a1, vec![], vec![], Vec::new());

        let view = state.channel_wallet_view(Some(b1));
        assert!(view.unfinalized.is_empty(), "orphaned branch entry pruned");
    }

    #[test]
    fn wallet_checkpoint_roundtrip() {
        let genesis = header_id(0);
        let mut state = TxState::new(genesis, MsgId::root());
        state.apply_finalized_note_ops(vec![wallet_note(1, 10), wallet_note(2, 20)]);

        let mut exported = state.channel_notes_base();
        exported.sort_by_key(|n| n.note_id);

        let mut restored = TxState::new(genesis, MsgId::root());
        restored.restore_channel_notes(exported.clone());
        let mut base = restored.channel_notes_base();
        base.sort_by_key(|n| n.note_id);
        assert_eq!(exported, base);

        let view = restored.channel_wallet_view(None);
        assert_eq!(view.finalized.len(), 2);
        assert!(view.unfinalized.is_empty());
    }

    #[test]
    fn pending_txs_orders_chained_bundles_parent_before_child() {
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(tip, genesis, genesis, vec![], vec![], Vec::new());

        let mut parent = MsgId::root();
        let mut hashes = Vec::new();
        for data in 1..=6u8 {
            let (bundle, inscribe_msg, _config_msg) = bundle_tx(parent, data);
            hashes.push(bundle.mantle_tx().hash());
            state.submit_other(bundle, channel_id);
            parent = inscribe_msg;
        }

        let resubmit: Vec<TxHash> = state.pending_txs(tip).iter().map(|(h, _)| *h).collect();
        assert_eq!(
            resubmit, hashes,
            "resubmission must return chained bundles parent-before-child"
        );
        let checkpoint: Vec<TxHash> = state.all_pending_txs().iter().map(|(h, _)| *h).collect();
        assert_eq!(
            checkpoint, hashes,
            "checkpoint serialization must preserve bundle submission order"
        );
    }

    /// The chain walk composes across kinds: pending inscription, then a
    /// bundle chained on it, then another pending inscription on the bundle's
    /// inscription tip.
    #[test]
    fn publish_parent_walks_mixed_pending_chain() {
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(tip, genesis, genesis, vec![], vec![], Vec::new());

        state.submit_inscription(make_dummy_tx(1), MsgId::root(), msg_id(10), [1].into());
        let (bundle, inscribe_msg, _config_msg) = bundle_tx(msg_id(10), 2);
        state.submit_other(bundle, channel_id);
        state.submit_inscription(make_dummy_tx(3), inscribe_msg, msg_id(30), [3].into());

        assert_eq!(state.publish_parent(tip), msg_id(30));
    }

    /// A contested position (pending inscription and bundle both claiming the
    /// same parent) stops the walk before the conflict, as for competing
    /// inscriptions.
    #[test]
    fn publish_parent_stops_at_contested_bundle_position() {
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(tip, genesis, genesis, vec![], vec![], Vec::new());

        state.submit_inscription(make_dummy_tx(1), MsgId::root(), msg_id(10), [1].into());
        let (bundle, _i, _c) = bundle_tx(MsgId::root(), 2);
        state.submit_other(bundle, channel_id);

        assert_eq!(
            state.publish_parent(tip),
            MsgId::root(),
            "walk must stop before a contested position"
        );
    }

    /// A pure config cut (no inscription in the tx) is NOT chained: it lands
    /// whenever it lands and orphans what it cut off — publishes keep
    /// chaining on the existing pending chain meanwhile.
    #[test]
    fn publish_parent_ignores_pending_pure_config_cut() {
        use lb_core::mantle::{
            channel::{SlotTimeframe, SlotTimeout},
            ops::channel::config::{ChannelConfigOp, Keys},
        };
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(tip, genesis, genesis, vec![], vec![], Vec::new());

        state.submit_inscription(make_dummy_tx(1), MsgId::root(), msg_id(10), [1].into());

        let config = ChannelConfigOp {
            channel: [0u8; 32].into(),
            parent: MsgId::root(),
            keys: Keys::try_from(vec![Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap()]).unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let config_tx = SignedMantleTx::new(
            RawMantleTx([Op::ChannelConfig(config)].into()),
            OpsProofs::empty(),
        );
        state.submit_other(config_tx, channel_id);

        assert_eq!(
            state.publish_parent(tip),
            msg_id(10),
            "a pure config must not divert the publish chain"
        );
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
        state.process_block(a1, genesis, genesis, vec![], vec![], Vec::new());

        // Build fork from a1 (before lib advances past a1)
        state.process_block(b1, a1, genesis, vec![], vec![], Vec::new());
        state.process_block(b2, b1, genesis, vec![], vec![], Vec::new());

        // Verify fork blocks exist before lib advances
        assert!(state.block_states.contains_key(&b1));
        assert!(state.block_states.contains_key(&b2));

        // Continue main chain, lib advances to a3
        state.process_block(a2, a1, genesis, vec![], vec![], Vec::new());
        state.process_block(a3, a2, a3, vec![], vec![], Vec::new()); // lib advances to a3

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
        state.process_block(a4, a3, a3, vec![], vec![], Vec::new());
        state.process_block(a5, a4, a5, vec![], vec![], Vec::new()); // lib advances to a5
        state.process_block(a6, a5, a5, vec![], vec![], Vec::new());

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
        let hash = tx.mantle_tx().hash();
        state.submit_inscription(tx, parent_msg, this_msg, [data].into());
        hash
    }

    /// Build a pure `[config]` tx for the zero channel; `data` varies the
    /// payload so ids differ.
    fn config_tx(parent: MsgId, data: u32) -> (SignedMantleTx<Unverified>, MsgId) {
        use lb_core::mantle::{
            channel::{SlotTimeframe, SlotTimeout},
            ops::channel::config::{ChannelConfigOp, Keys},
        };
        let config = ChannelConfigOp {
            channel: [0u8; 32].into(),
            parent,
            keys: Keys::try_from(vec![Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap()]).unwrap(),
            posting_timeframe: SlotTimeframe::from(data),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let config_msg = config.id();
        let tx = SignedMantleTx::new(
            RawMantleTx([Op::ChannelConfig(config)].into()),
            OpsProofs::empty(),
        );
        (tx, config_msg)
    }

    /// Wrap a pure config tx as it is classified when mined: a clean
    /// [`BlockChannelTx::Config`] on the config lineage (the mixed/unknown
    /// `Custom { config_entries }` shape is exercised separately in
    /// `mixed_config_tx_is_custom_but_advances_the_config_tip`).
    fn config_block_tx(
        tx: &SignedMantleTx<Unverified>,
        this_msg: MsgId,
        parent: MsgId,
    ) -> BlockChannelTx {
        BlockChannelTx::Config(InscriptionInfo {
            tx_hash: tx.mantle_tx().hash(),
            parent_msg: parent,
            this_msg,
            payload: [].into(),
        })
    }

    /// Once a rival config lands on-branch and moves the config tip, our
    /// pending config chained on the superseded parent is shed.
    #[test]
    fn shed_stale_pending_configs_removes_superseded_config() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());

        // Our pending config chains on the (root) config tip.
        let (stale, _) = config_tx(MsgId::root(), 1);
        let stale_hash = stale.mantle_tx().hash();
        state.submit_other(stale, channel_id);

        // No config has landed yet — the local config tip is root, so ours is
        // still viable.
        state.process_block(b1, genesis, genesis, vec![], vec![], Vec::new());
        assert!(state.shed_stale_pending_configs(b1).is_empty());
        assert!(state.pending_other_contains(&stale_hash));

        // A rival config (also chaining on root) lands on-branch and moves the
        // config tip: our pending config's parent is now superseded → shed.
        let (rival, rival_msg) = config_tx(MsgId::root(), 2);
        state.process_block(
            b2,
            b1,
            genesis,
            vec![],
            vec![config_block_tx(&rival, rival_msg, MsgId::root())],
            Vec::new(),
        );

        let shed = state.shed_stale_pending_configs(b2);
        assert_eq!(shed.len(), 1);
        assert_eq!(shed[0].mantle_tx().hash(), stale_hash);
        assert!(!state.pending_other_contains(&stale_hash));
    }

    /// The finalized config tip must survive a warm restart. It lives only in
    /// `finalized_config` once its block prunes below LIB, so a checkpoint has
    /// to carry it — otherwise `config_tip_at` falls back to `root` after
    /// restart and a later config would chain on a stale parent.
    #[test]
    fn finalized_config_survives_checkpoint_restore() {
        let genesis = header_id(0);
        let config = msg_id(7);

        // A config finalized at/below LIB; the checkpoint captures its tip.
        let mut state = TxState::new(genesis, MsgId::root());
        state.set_finalized_config(config);
        assert_eq!(state.finalized_config(), config);

        // Warm restart without restoring it: `config_tip_at` falls back to root
        // (the bug — a new config would chain on the wrong parent).
        let mut restored = TxState::new(genesis, MsgId::root());
        assert_eq!(restored.config_tip_at(genesis), MsgId::root());

        // Restored from the checkpoint's `finalized_config`, the tip resolves.
        restored.set_finalized_config(state.finalized_config());
        assert_eq!(restored.config_tip_at(genesis), config);
    }

    /// Many configs can land in one block as a chain (`root → C1 → C2`). The
    /// config walk must resolve the *tip* of that chain, not the first entry,
    /// so the shed evaluates pending configs against the correct parent.
    #[test]
    fn config_chain_in_one_block_resolves_tip_and_sheds_superseded() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());

        // Two chained configs land in a single block: C1 on root, C2 on C1.
        let (c1, c1_msg) = config_tx(MsgId::root(), 1);
        let (c2, c2_msg) = config_tx(c1_msg, 2);

        // One pending config chains on the now-superseded root; another chains
        // on the chain tip C2.
        let (stale, _) = config_tx(MsgId::root(), 3);
        let stale_hash = stale.mantle_tx().hash();
        let (on_tip, _) = config_tx(c2_msg, 4);
        let on_tip_hash = on_tip.mantle_tx().hash();
        state.submit_other(stale, channel_id);
        state.submit_other(on_tip, channel_id);

        state.process_block(
            b1,
            genesis,
            genesis,
            vec![],
            vec![
                config_block_tx(&c1, c1_msg, MsgId::root()),
                config_block_tx(&c2, c2_msg, c1_msg),
            ],
            Vec::new(),
        );

        // The walk resolves the chain tip (C2), not C1 or root.
        assert_eq!(
            state.config_tip_at(b1),
            c2_msg,
            "config_tip_at must resolve the last config in the block's chain"
        );

        // The shed evaluates against that tip: the root-parented config is
        // superseded and shed; the C2-parented one still chains on the tip and
        // is kept.
        let shed = state.shed_stale_pending_configs(b1);
        assert_eq!(shed.len(), 1);
        assert_eq!(shed[0].mantle_tx().hash(), stale_hash);
        assert!(!state.pending_other_contains(&stale_hash));
        assert!(state.pending_other_contains(&on_tip_hash));
    }

    /// A pending config mined on the current branch sits in the tip's safe set
    /// and is not shed.
    #[test]
    fn shed_stale_pending_configs_keeps_safe_on_branch_config() {
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());

        let (config, config_msg) = config_tx(MsgId::root(), 1);
        let config_hash = config.mantle_tx().hash();
        // Build the block entry before `submit_other` moves the tx.
        let block_tx = config_block_tx(&config, config_msg, MsgId::root());
        state.submit_other(config, channel_id);

        // The config lands on-branch: in the block's safe set and on the config
        // lineage.
        state.process_block(
            tip,
            genesis,
            genesis,
            vec![config_hash],
            vec![block_tx],
            Vec::new(),
        );

        assert!(state.shed_stale_pending_configs(tip).is_empty());
        assert!(state.pending_other_contains(&config_hash));
    }

    /// Regression (youngjoon): a pending config that merely extends our local
    /// config tip must survive while its own block is still unprocessed — even
    /// though the node may report a further-ahead config tip. We seed from the
    /// local tip, so an ahead-of-us node tip never sheds it.
    #[test]
    fn shed_stale_pending_configs_keeps_pending_extending_local_tip() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());

        // Config B lands on-branch → the local config tip is B.
        let (b_config, b_msg) = config_tx(MsgId::root(), 1);
        state.process_block(
            b1,
            genesis,
            genesis,
            vec![],
            vec![config_block_tx(&b_config, b_msg, MsgId::root())],
            Vec::new(),
        );

        // Our config C chains on B and is pending; its block hasn't arrived, so
        // it is in no safe set.
        let (c_config, _c_msg) = config_tx(b_msg, 2);
        let c_hash = c_config.mantle_tx().hash();
        state.submit_other(c_config, channel_id);

        // C extends the local tip B, so it survives.
        assert!(state.shed_stale_pending_configs(b1).is_empty());
        assert!(state.pending_other_contains(&c_hash));
    }

    /// A config landing sheds only the not-yet-mined pending tail; an
    /// inscription already mined on this branch is kept (re-posting an on-chain
    /// entry would duplicate). Same config tip on a later block sheds nothing.
    #[test]
    fn config_land_sheds_pending_tail_but_keeps_mined_inscription() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        // p1 is published and mined on-branch — it enters the block's safe set.
        let p1 = submit_fake_inscription(&mut state, 1, MsgId::root(), msg_id(1));
        let m1 = InscriptionInfo {
            tx_hash: p1,
            parent_msg: MsgId::root(),
            this_msg: msg_id(1),
            payload: [1].into(),
        };
        state.process_block(
            b1,
            genesis,
            genesis,
            vec![p1],
            vec![BlockChannelTx::Inscription(m1)],
            Vec::new(),
        );

        // p2, p3 chain on the mined tip but are not yet mined (pending).
        let p2 = submit_fake_inscription(&mut state, 2, msg_id(1), msg_id(2));
        let p3 = submit_fake_inscription(&mut state, 3, msg_id(2), msg_id(3));

        // No config has landed → the config tip is unchanged → nothing shed.
        assert!(state.shed_pending_inscriptions_on_config(b1).is_empty());

        // A config lands, moving the config tip.
        let (cfg, cfg_msg) = config_tx(MsgId::root(), 9);
        let b2 = header_id(2);
        state.process_block(
            b2,
            b1,
            genesis,
            vec![],
            vec![config_block_tx(&cfg, cfg_msg, MsgId::root())],
            Vec::new(),
        );

        // Only the not-yet-mined tail is shed, parent-first; the mined p1 is on
        // chain and must not be orphaned (re-posting it would duplicate).
        let shed: Vec<TxHash> = state
            .shed_pending_inscriptions_on_config(b2)
            .iter()
            .map(PendingTx::tx_hash)
            .collect();
        assert_eq!(shed, vec![p2, p3], "shed only the not-yet-mined tail");
        assert!(
            state.pending_inscription(&p1).is_some(),
            "the mined inscription must not be orphaned"
        );

        // Same config tip on the next call → nothing more to shed.
        assert!(state.shed_pending_inscriptions_on_config(b2).is_empty());
    }

    /// A config-only block does not touch the message lineage: no update is
    /// reported, the channel tip stays, and pending inscriptions survive.
    #[test]
    fn config_only_block_does_not_shed_pending_inscription() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        // Mined inscription M establishes channel tip m.
        let m_info = InscriptionInfo {
            tx_hash: make_dummy_tx(1).mantle_tx().hash(),
            parent_msg: MsgId::root(),
            this_msg: msg_id(1),
            payload: [1].into(),
        };
        state.process_block(
            b1,
            genesis,
            genesis,
            vec![],
            vec![BlockChannelTx::Inscription(m_info)],
            Vec::new(),
        );

        // Local pending inscription P chained on m (published, not mined).
        let p_hash = submit_fake_inscription(&mut state, 2, msg_id(1), msg_id(2));

        let old_lineage = state.channel_lineage(b1);

        // A config-only block classifies to no channel txs at all.
        state.process_block(b2, b1, genesis, vec![], vec![], Vec::new());

        assert!(
            state.detect_channel_update(&old_lineage, b2).is_none(),
            "a config-only block does not change the message lineage"
        );
        assert_eq!(state.channel_tip_at(b2), msg_id(1));
        assert!(state.pending_txs(b2).iter().any(|(h, _)| *h == p_hash));
        assert!(state.shed_off_branch_pending(b2).is_empty());
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

        state.process_block(block1, genesis, genesis, vec![], vec![], Vec::new());

        // Capture the old lineage before inserting block2, mirroring the real
        // caller; computing it after would let c1 bridge into the "before" view.
        let old_lineage = state.channel_lineage(block1);

        let c1_msg = msg_id(20);
        let c1_tx = make_dummy_tx(99);
        let c1_tx_hash = c1_tx.mantle_tx().hash();
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
            Vec::new(),
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

        state.process_block(block1, genesis, genesis, vec![], vec![], Vec::new());

        // Capture the old lineage before inserting block2, mirroring the real
        // caller; computing it after would let c1 bridge into the "before" view.
        let old_lineage = state.channel_lineage(block1);

        let c1_msg = msg_id(20);
        let c1_inscription = InscriptionInfo {
            tx_hash: make_dummy_tx(99).mantle_tx().hash(),
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
            Vec::new(),
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

        state.process_block(block1, genesis, genesis, vec![], vec![], Vec::new());

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

        state.process_block(block1, genesis, genesis, vec![], vec![], Vec::new());

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

        state.process_block(block1, genesis, genesis, vec![], vec![], Vec::new());

        // c1 lands, consuming root
        let c1_msg = msg_id(20);
        let c1_inscription = InscriptionInfo {
            tx_hash: make_dummy_tx(99).mantle_tx().hash(),
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
            Vec::new(),
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
        let hash1 = tx1.mantle_tx().hash();
        let hash2 = tx2.mantle_tx().hash();

        state.submit_other(tx1, ChannelId::from([0u8; 32]));
        state.submit_other(tx2, ChannelId::from([0u8; 32]));

        // b1 has tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![], Vec::new());
        // b2 has tx2
        state.process_block(b2, b1, genesis, vec![hash2], vec![], Vec::new());
        // b3, lib jumps from genesis to b2 (skipping b1)
        state.process_block(b3, b2, b2, vec![], vec![], Vec::new());
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
