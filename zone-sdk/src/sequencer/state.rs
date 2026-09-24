use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps,
        ledger::{Inputs, NoteId, Outputs, verification_mode::StandardMode},
        ops::{
            OpRef,
            channel::{ChannelId, MsgId, inscribe::Inscription},
        },
        traits::Hashable as _,
        transactions::{hash::TxHash, states::Unverified},
    },
};
use lb_key_management_system_service::keys::UnverifiedEd25519PublicKey;
use rpds::HashTrieSetSync;

/// The Ed25519 author of a tx's channel inscription op, if it carries one — the
/// signer stored on the pending entry's `signed_tx`, recovered for lineage
/// reconstruction.
fn inscription_signer(
    tx: &SignedOps<Unverified, StandardMode>,
) -> Option<UnverifiedEd25519PublicKey> {
    tx.op_refs_iter().find_map(|op| match op {
        OpRef::ChannelInscribe(inscribe) => Some(inscribe.signer),
        _ => None,
    })
}

use super::{
    block_fetch::{channel_configs, channel_inscriptions, channel_transfers, is_pure_config},
    channel_wallet::{ChannelWallet, NoteOp},
    types::{
        AtomicWithdrawInfo, ChannelNote, ChannelUpdateTx, ChannelWalletView, Error,
        InscriptionInfo, PendingTx, PinDepositInfo, TxSource, WithdrawInfo,
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
    signed_tx: SignedOps<Unverified, StandardMode>,
    /// The tx's channel inscriptions in op order.
    infos: Vec<InscriptionInfo>,
    /// The tx's channel configs in op order, on the config lineage.
    config_infos: Vec<InscriptionInfo>,
    /// Classified as a single config with at most one funding transfer, the
    /// shape reported as [`ChannelUpdateTx::Config`].
    pure_config: bool,
    first_parent: Option<MsgId>,
    last_msg: Option<MsgId>,
    config_parent: Option<MsgId>,
    last_config: Option<MsgId>,
    /// Submission order, for checkpoint serialization.
    seq: u64,
}

/// Where an opaque tx sits in the two lineages, see [`PendingOtherTx`].
#[derive(Clone, Copy)]
struct OpaqueLineage {
    /// Parent of the tx's first inscription: where it attaches to the
    /// message lineage.
    first_parent: Option<MsgId>,
    /// Id of the tx's last inscription: the message tip it leaves behind.
    last_msg: Option<MsgId>,
    /// Parent of the tx's first config: where it attaches to the config
    /// lineage.
    config_parent: Option<MsgId>,
    /// Id of the tx's last config: the config tip it leaves behind.
    last_config: Option<MsgId>,
}

fn opaque_lineage(
    tx: &SignedOps<Unverified, StandardMode>,
    channel_id: ChannelId,
) -> OpaqueLineage {
    let mut first_parent = None;
    let mut last_msg = None;
    let mut config_parent = None;
    let mut last_config = None;
    for op in tx.op_refs_iter() {
        match op {
            OpRef::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                if last_msg.is_none() {
                    first_parent = Some(inscribe.parent);
                }
                last_msg = Some(inscribe.id());
            }
            OpRef::ChannelConfig(config) if config.channel == channel_id => {
                if last_config.is_none() {
                    config_parent = Some(config.parent);
                }
                last_config = Some(config.id());
            }
            _ => {}
        }
    }
    OpaqueLineage {
        first_parent,
        last_msg,
        config_parent,
        last_config,
    }
}

/// A local submission chained on a channel position that already has a
/// pending continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentTaken {
    pub parent: MsgId,
    pub by: TxHash,
}

impl std::fmt::Display for ParentTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "parent {} already has pending continuation {}",
            hex::encode(self.parent.as_ref()),
            hex::encode(self.by.0)
        )
    }
}

impl From<ParentTaken> for Error {
    fn from(taken: ParentTaken) -> Self {
        Self::ChannelStateChanged(taken.to_string())
    }
}

/// The bundle nature of a pending inscription. Lets us surface the right
/// [`PendingTx`] variant on finalize/adopt and re-prepare on orphan.
#[derive(Debug, Clone)]
pub enum PendingBundle {
    /// A plain inscription.
    Plain,
    /// An atomic inscription+withdraw bundle: the tx's `Op::ChannelWithdraw`
    /// ops (in tx order) plus the recipient notes it releases (for re-issue
    /// from an orphan report).
    Withdraw {
        withdraws: Vec<WithdrawInfo>,
        outputs: Outputs,
    },
    /// An atomic inscription+transfer bundle pinning a deposit: the channel
    /// notes the bundled transfer consumes.
    PinDeposit(Inputs),
}

/// Local pending inscription with lineage metadata.
#[derive(Debug, Clone)]
pub struct PendingInscription {
    pub tx_hash: TxHash,
    pub signed_tx: SignedOps<Unverified, StandardMode>,
    pub parent_msg: MsgId,
    pub this_msg: MsgId,
    pub payload: Inscription,
    pub bundle: PendingBundle,
    pub posted: bool,
}

impl PendingInscription {
    fn info(&self) -> InscriptionInfo {
        InscriptionInfo {
            tx_hash: self.tx_hash,
            parent_msg: self.parent_msg,
            this_msg: self.this_msg,
            payload: self.payload.clone(),
            signer: inscription_signer(&self.signed_tx),
        }
    }
}

/// Transaction state tracker.
pub struct TxState {
    /// Local pending inscriptions indexed by tx hash.
    pending: HashMap<TxHash, PendingInscription>,
    /// Parent `MsgId` → the one pending tx chaining from it, plain or
    /// opaque. A channel position has a single pending continuation: a local
    /// submit on a taken parent is refused, a mined entry displaces it.
    pending_by_parent: HashMap<MsgId, TxHash>,
    /// Pending entries displaced by a mined sibling, awaiting the shed pass
    /// that reports them orphaned.
    displaced: Vec<PendingTx>,
    displaced_other: Vec<SignedOps<Unverified, StandardMode>>,
    /// Opaque pending txs, ours or mirrored: retried byte-identically until
    /// finalized or shed.
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
    /// Per L1 block channel content (unfinalized window), canonical or not.
    block_txs: HashMap<HeaderId, StoredBlock>,
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

impl TxState {
    #[must_use]
    pub fn new(lib: HeaderId, finalized_msg: MsgId) -> Self {
        let mut block_states = BTreeMap::new();
        block_states.insert(lib, HashTrieSetSync::new_sync());
        Self {
            pending: HashMap::new(),
            pending_by_parent: HashMap::new(),
            displaced: Vec::new(),
            displaced_other: Vec::new(),
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
    ///
    /// Also advances `observed_config_tip` to it: an already-finalized config
    /// has effectively been observed, so the config-driven inscription shed
    /// must not treat it as a fresh landing and orphan the pending tail on the
    /// first block after resume/backfill.
    pub const fn set_finalized_config(&mut self, config: MsgId) {
        self.finalized_config = config;
        self.observed_config_tip = config;
    }

    /// Submit an inscription tx for tracking with lineage metadata. Use
    /// [`Self::submit_atomic_withdraw`] for inscription+withdraw bundles.
    pub fn submit_inscription(
        &mut self,
        signed_tx: SignedOps<Unverified, StandardMode>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
    ) -> Result<(), ParentTaken> {
        self.insert_pending(
            signed_tx,
            parent_msg,
            this_msg,
            payload,
            PendingBundle::Plain,
        )
    }

    /// Submit an atomic inscription+withdraw bundle for tracking. `withdraws`
    /// must mirror the `Op::ChannelWithdraw` ops in the bundle, in tx order;
    /// `outputs` are the recipient notes it releases (for orphan re-issue).
    pub fn submit_atomic_withdraw(
        &mut self,
        signed_tx: SignedOps<Unverified, StandardMode>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        withdraws: Vec<WithdrawInfo>,
        outputs: Outputs,
    ) -> Result<(), ParentTaken> {
        self.insert_pending(
            signed_tx,
            parent_msg,
            this_msg,
            payload,
            PendingBundle::Withdraw { withdraws, outputs },
        )
    }

    /// Submit an atomic inscription+transfer bundle pinning a deposit.
    /// `consumed_notes` mirrors the bundled transfer's input notes (the
    /// deposited notes being pinned).
    pub fn submit_pin_deposit(
        &mut self,
        signed_tx: SignedOps<Unverified, StandardMode>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        consumed_notes: Inputs,
    ) -> Result<(), ParentTaken> {
        self.insert_pending(
            signed_tx,
            parent_msg,
            this_msg,
            payload,
            PendingBundle::PinDeposit(consumed_notes),
        )
    }

    fn insert_pending(
        &mut self,
        signed_tx: SignedOps<Unverified, StandardMode>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        bundle: PendingBundle,
    ) -> Result<(), ParentTaken> {
        let tx_hash = signed_tx.hash();
        if let Some(by) = self.pending_child(parent_msg)
            && by != tx_hash
        {
            return Err(ParentTaken {
                parent: parent_msg,
                by,
            });
        }
        self.track_local_tx(tx_hash);
        self.pending_by_parent.insert(parent_msg, tx_hash);
        self.pending.insert(
            tx_hash,
            PendingInscription {
                tx_hash,
                signed_tx,
                parent_msg,
                this_msg,
                payload,
                bundle,
                posted: false,
            },
        );
        Ok(())
    }

    /// Track an inscription observed on the canonical channel (ours or
    /// another sequencer's) so the pending set mirrors the channel view
    /// above LIB: a reorged-out entry whose lineage still reaches the
    /// channel tip is retried byte-identically via [`Self::pending_txs`],
    /// no matter who authored it. No-op when the tx is already tracked.
    ///
    /// A pending continuation already sitting on the same parent lost the
    /// position to this mined entry: it and everything chained on it are
    /// displaced, to be reported orphaned by the next shed pass.
    ///
    /// `bundle` classifies the tx (plain inscription, atomic withdraw, or
    /// pin deposit), matching the `submit_*` classification.
    /// Observed entries start `posted` — they were seen on chain, so they never
    /// count as first-time publishes.
    pub fn observe_channel_inscription(
        &mut self,
        signed_tx: SignedOps<Unverified, StandardMode>,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Inscription,
        bundle: PendingBundle,
    ) {
        let tx_hash = signed_tx.hash();
        if self.is_tracked(&tx_hash) {
            return;
        }
        if let Some(sibling) = self.pending_child(parent_msg) {
            self.displace_chain(sibling);
        }
        self.pending_by_parent.insert(parent_msg, tx_hash);
        self.pending.insert(
            tx_hash,
            PendingInscription {
                tx_hash,
                signed_tx,
                parent_msg,
                this_msg,
                payload,
                bundle,
                posted: true,
            },
        );
    }

    /// The pending tx chaining from `parent`, if any.
    fn pending_child(&self, parent: MsgId) -> Option<TxHash> {
        self.pending_by_parent.get(&parent).copied()
    }

    /// The message a pending tx leaves as the channel tip: a plain entry's
    /// own id, an opaque tx's last inscription.
    fn pending_tip_of(&self, tx_hash: TxHash) -> Option<MsgId> {
        if let Some(pending) = self.pending.get(&tx_hash) {
            return Some(pending.this_msg);
        }
        self.pending_other.get(&tx_hash)?.last_msg
    }

    /// Remove `head` and everything pending chained after it, queuing them
    /// for orphan reporting.
    fn displace_chain(&mut self, head: TxHash) {
        let mut next = Some(head);
        while let Some(tx_hash) = next {
            next = self
                .pending_tip_of(tx_hash)
                .and_then(|tip| self.pending_child(tip));
            if let Some(entry) = self.pending_tx_of(&tx_hash) {
                self.displaced.push(entry);
                self.remove_pending(&tx_hash);
            } else if let Some(signed_tx) = self.remove_pending(&tx_hash) {
                self.displaced_other.push(signed_tx);
            }
        }
    }

    /// The reportable form of a pending inscription or bundle.
    fn pending_tx_of(&self, tx_hash: &TxHash) -> Option<PendingTx> {
        let pending = self.pending.get(tx_hash)?;
        let info = pending.info();
        Some(match &pending.bundle {
            PendingBundle::Withdraw { withdraws, outputs } => {
                PendingTx::AtomicWithdraw(AtomicWithdrawInfo {
                    tx_hash: pending.tx_hash,
                    inscription: info,
                    withdraws: withdraws.clone(),
                    outputs: outputs.clone(),
                })
            }
            PendingBundle::PinDeposit(consumed_notes) => PendingTx::PinDeposit(PinDepositInfo {
                tx_hash: pending.tx_hash,
                inscription: info,
                consumed_notes: consumed_notes.clone(),
            }),
            PendingBundle::Plain => PendingTx::Inscription(info),
        })
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
        signed_tx: SignedOps<Unverified, StandardMode>,
        channel_id: ChannelId,
    ) -> Result<Option<MsgId>, ParentTaken> {
        let tx_hash = signed_tx.hash();
        let lineage = opaque_lineage(&signed_tx, channel_id);
        if let Some(parent) = lineage.first_parent
            && let Some(by) = self.pending_child(parent)
            && by != tx_hash
        {
            return Err(ParentTaken { parent, by });
        }
        self.track_local_tx(tx_hash);
        Ok(self.insert_other(signed_tx, channel_id, lineage))
    }

    /// Track an opaque tx observed on the canonical channel, the counterpart
    /// of [`Self::observe_channel_inscription`]: no-op when already tracked,
    /// displaces a pending continuation on its message parent.
    pub fn observe_other_tx(
        &mut self,
        signed_tx: SignedOps<Unverified, StandardMode>,
        channel_id: ChannelId,
    ) {
        let tx_hash = signed_tx.hash();
        if self.is_tracked(&tx_hash) {
            return;
        }
        let lineage = opaque_lineage(&signed_tx, channel_id);
        if let Some(parent) = lineage.first_parent
            && let Some(sibling) = self.pending_child(parent)
        {
            self.displace_chain(sibling);
        }
        self.insert_other(signed_tx, channel_id, lineage);
    }

    fn insert_other(
        &mut self,
        signed_tx: SignedOps<Unverified, StandardMode>,
        channel_id: ChannelId,
        lineage: OpaqueLineage,
    ) -> Option<MsgId> {
        let tx_hash = signed_tx.hash();
        let OpaqueLineage {
            first_parent,
            last_msg,
            config_parent,
            last_config,
        } = lineage;
        let infos = channel_inscriptions(&signed_tx, channel_id);
        let config_infos = channel_configs(&signed_tx, channel_id);
        let pure_config = is_pure_config(&signed_tx, channel_id);
        if let Some(parent) = first_parent {
            self.pending_by_parent.insert(parent, tx_hash);
        }
        let seq = self.next_other_seq;
        self.next_other_seq += 1;
        self.pending_other.insert(
            tx_hash,
            PendingOtherTx {
                signed_tx,
                infos,
                config_infos,
                pure_config,
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
        //
        // Every channel-tip-touching tx joins the set, tracked or not: a fork
        // block is not mirrored on arrival, and its txs must read as mined
        // once the branch turns canonical and the store re-mirrors them.
        let mut safe_set = self
            .block_states
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();

        for tx in our_txs {
            safe_set = safe_set.insert(tx);
        }
        self.block_states.insert(block_id, safe_set);

        // Store the block's classified channel txs
        if !channel_txs.is_empty() {
            self.block_txs.entry(block_id).or_default().channel_txs = channel_txs;
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
    /// parent before any child: inscriptions along the pending chain from
    /// the channel tip, opaque txs by submission order (`seq`) — a
    /// locally chained bundle can only be built after the bundle that
    /// establishes its parent tip, so submission order is dependency order.
    pub fn pending_txs(&self, tip: HeaderId) -> Vec<(TxHash, SignedOps<Unverified, StandardMode>)> {
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

    /// Remove pending inscriptions whose lineage does NOT reach the current
    /// channel tip and that aren't already in a block on this branch, along
    /// with entries a mined sibling displaced since the last pass.
    /// Returns the removed entries in **parent-before-child order** so a
    /// consumer that iterates and republishes naturally rebuilds the chain
    /// in dependency order.
    ///
    /// Bundle-aware: atomic inscription+withdraw bundles are returned as
    /// [`PendingTx::AtomicWithdraw`] so the caller can re-prepare them with
    /// a fresh `parent_msg`; plain inscriptions are returned
    /// as [`PendingTx::Inscription`].
    pub fn shed_off_branch_pending(&mut self, tip: HeaderId) -> Vec<PendingTx> {
        let mut shed = std::mem::take(&mut self.displaced);
        if self.pending.is_empty() {
            return shed;
        }
        let channel_tip = self.channel_tip_at(tip);
        let on_branch: HashSet<TxHash> = self
            .collect_pending_suffix(channel_tip)
            .iter()
            .map(|i| i.tx_hash)
            .collect();
        let safe = self.safe_at(tip);

        let eligible: HashSet<TxHash> = self
            .pending
            .keys()
            .filter(|h| !on_branch.contains(h) && !safe.contains(h))
            .copied()
            .collect();
        shed.extend(self.drain_pending_in_lineage_order(&eligible));
        shed
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

        // Walk the chain from each root parent; collect only eligible
        // entries in parent-first order.
        let mut ordered = Vec::with_capacity(eligible.len());
        let mut seen = HashSet::new();
        for root in root_parents {
            for info in self.collect_pending_suffix(root) {
                if eligible.contains(&info.tx_hash)
                    && seen.insert(info.tx_hash)
                    && let Some(entry) = self.pending_tx_of(&info.tx_hash)
                {
                    ordered.push(entry);
                }
            }
        }

        for entry in &ordered {
            self.remove_pending(&entry.tx_hash());
        }
        ordered
    }

    /// Tx hashes mined on `tip`'s branch (its cumulative safe set).
    fn safe_at(&self, tip: HeaderId) -> HashSet<TxHash> {
        self.block_states
            .get(&tip)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Blocks on `tip`'s branch strictly above LIB, oldest first.
    fn branch_blocks_above_lib(&self, tip: HeaderId) -> Vec<HeaderId> {
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

    /// Pending bundles chaining from `tip`'s channel tip that are not mined
    /// on its branch, in lineage order.
    fn unmined_bundles_in_suffix(
        &self,
        tip: HeaderId,
    ) -> Vec<(InscriptionInfo, &PendingInscription)> {
        let safe = self.safe_at(tip);
        self.collect_pending_suffix(self.channel_tip_at(tip))
            .into_iter()
            .filter(|info| !safe.contains(&info.tx_hash))
            .filter_map(|info| self.pending.get(&info.tx_hash).map(|p| (info, p)))
            .filter(|(_, p)| !matches!(p.bundle, PendingBundle::Plain))
            .collect()
    }

    /// Notes a new bundle may spend at `tip`: the branch view minus what
    /// un-mined pending bundles already consume. Their outputs are not
    /// offered.
    #[must_use]
    pub fn spendable_wallet_view(
        &self,
        tip: Option<HeaderId>,
        channel_id: ChannelId,
    ) -> ChannelWalletView {
        let mut view = self.channel_wallet_view(tip);
        let Some(tip) = tip else {
            return view;
        };
        let reserved: HashSet<NoteId> = self
            .unmined_bundles_in_suffix(tip)
            .into_iter()
            .flat_map(|(_, p)| channel_transfers(&p.signed_tx, channel_id))
            .flat_map(|t| t.inputs.iter().copied())
            .collect();
        view.finalized.retain(|n| !reserved.contains(&n.note_id));
        view.unfinalized.retain(|n| !reserved.contains(&n.note_id));
        view
    }

    /// Shed pending bundles whose transfer inputs are gone from this branch
    /// (e.g. their deposit reorged out) — they can never land — together with
    /// every pending entry chaining on them, parent first, as typed
    /// [`PendingTx`].
    pub fn shed_bundles_with_missing_inputs(
        &mut self,
        tip: HeaderId,
        channel_id: ChannelId,
    ) -> Vec<PendingTx> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let view = self.channel_wallet_view(Some(tip));
        let mut available: HashSet<NoteId> = view
            .finalized
            .iter()
            .chain(view.unfinalized.iter())
            .map(|n| n.note_id)
            .collect();
        let mut shed: HashSet<TxHash> = HashSet::new();
        for (info, pending) in self.unmined_bundles_in_suffix(tip) {
            let transfers: Vec<_> = channel_transfers(&pending.signed_tx, channel_id).collect();
            if transfers
                .iter()
                .all(|t| t.inputs.iter().all(|id| available.contains(id)))
            {
                available.extend(transfers.iter().flat_map(|t| t.utxos().map(|u| u.id())));
            } else {
                // A bundle that cannot land takes everything chained on it.
                shed.insert(info.tx_hash);
                shed.extend(
                    self.collect_pending_suffix(info.this_msg)
                        .iter()
                        .map(|child| child.tx_hash),
                );
            }
        }
        self.drain_pending_in_lineage_order(&shed)
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

        let safe = self.safe_at(tip);
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
    ) -> Vec<SignedOps<Unverified, StandardMode>> {
        let mut displaced = std::mem::take(&mut self.displaced_other);
        if self.pending_other.is_empty() {
            return displaced;
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
        let safe = self.safe_at(tip);

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
        displaced.extend(
            shed.into_iter()
                .filter_map(|hash| self.remove_pending(&hash)),
        );
        displaced
    }

    /// Shed pending config-carrying txs whose config parent can no longer
    /// reach the mined config tip: removed from retry and returned whole for
    /// orphan reporting.
    pub fn shed_stale_pending_configs(
        &mut self,
        tip: HeaderId,
    ) -> Vec<SignedOps<Unverified, StandardMode>> {
        if self.pending_other.is_empty() {
            return Vec::new();
        }
        // Seed with the config tip we have actually processed on this branch —
        // not the node's config tip, which can race ahead of the blocks we have
        // processed and falsely orphan a pending config that merely extends our
        // local tip (its block just hasn't arrived yet).
        let landable = self.landable_configs(self.config_tip_at(tip));
        let safe = self.safe_at(tip);

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
    pub fn all_pending_txs(&self) -> Vec<(TxHash, SignedOps<Unverified, StandardMode>)> {
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
    pub fn remove_pending(
        &mut self,
        tx_hash: &TxHash,
    ) -> Option<SignedOps<Unverified, StandardMode>> {
        let (parent, signed_tx) = if let Some(removed) = self.pending.remove(tx_hash) {
            (Some(removed.parent_msg), removed.signed_tx)
        } else {
            let removed = self.pending_other.remove(tx_hash)?;
            (removed.first_parent, removed.signed_tx)
        };
        if let Some(parent) = parent
            && self.pending_by_parent.get(&parent) == Some(tx_hash)
        {
            self.pending_by_parent.remove(&parent);
        }
        Some(signed_tx)
    }

    /// Derive the publish parent from state: the tail of the pending chain
    /// off the canonical tip, or the canonical tip itself when nothing is
    /// pending.
    #[must_use]
    pub fn publish_parent(&self, tip: HeaderId) -> MsgId {
        let channel_tip = self.channel_tip_at(tip);
        self.pending_publish_tail(channel_tip)
            .unwrap_or(channel_tip)
    }

    /// The last message of the pending chain off `from_msg`; `None` when
    /// nothing is pending there.
    fn pending_publish_tail(&self, from_msg: MsgId) -> Option<MsgId> {
        let mut current = from_msg;
        let mut found_any = false;
        // Bounded by the pending population: a longer walk would mean a
        // cycle in (possibly inconsistent) pending lineage data.
        for _ in 0..=(self.pending.len() + self.pending_other.len()) {
            let Some(next) = self.pending_next(current) else {
                break;
            };
            current = next;
            found_any = true;
        }
        found_any.then_some(current)
    }

    /// The message the pending continuation of `current` leaves as tip, if
    /// one is pending and moves the tip.
    fn pending_next(&self, current: MsgId) -> Option<MsgId> {
        let next = self.pending_tip_of(self.pending_child(current)?)?;
        (next != current).then_some(next)
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

    /// Detect a channel update between old and new L1 tips.
    ///
    /// Diffs the two channel *lineages*, message and config entries alike.
    /// `old_lineage` must be captured by the caller via
    /// [`Self::channel_lineage`] **before** this event's block is inserted,
    /// so the "before" side isn't contaminated by the just-added block;
    /// `new_lineage` is computed here, after the insert.
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
    ///
    /// `finalized_now`: msg-ids finalized by this event, masked so a LIB jump
    /// never reads as an orphan.
    #[must_use]
    pub fn detect_channel_update(
        &self,
        old_lineage: &[InscriptionInfo],
        new_tip: HeaderId,
        finalized_now: &HashSet<MsgId>,
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
        finalized.extend(finalized_now.iter().copied());

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

    /// The reportable channel view at `tip` above the finalized boundary, in
    /// lineage order. `finalized_now`: msg-ids finalized by this event.
    #[must_use]
    pub fn channel_view_txs(
        &self,
        tip: HeaderId,
        finalized_now: &HashSet<MsgId>,
    ) -> Vec<ChannelUpdateTx> {
        let lineage = self.channel_lineage(tip);
        let mut finalized = self.finalized_prefix_ids(&lineage);
        finalized.extend(finalized_now.iter().copied());
        self.update_txs_from_infos(lineage.iter().filter(|i| !finalized.contains(&i.this_msg)))
    }

    /// The part of the view at `tip` that `tracked` entries chain on: those
    /// entries and every ancestor of theirs above LIB, in lineage order. A
    /// pending entry restored from a checkpoint proves its ancestors were the
    /// view before the restart.
    pub(super) fn lineage_under(
        &self,
        tip: HeaderId,
        tracked: &HashSet<TxHash>,
    ) -> Vec<InscriptionInfo> {
        let lineage = self.channel_lineage(tip);
        let mut known: HashSet<MsgId> = HashSet::new();
        for info in lineage.iter().rev() {
            if tracked.contains(&info.tx_hash) || known.contains(&info.this_msg) {
                known.insert(info.this_msg);
                known.insert(info.parent_msg);
            }
        }
        lineage
            .into_iter()
            .filter(|info| known.contains(&info.this_msg))
            .collect()
    }

    /// Msg-ids of `lineage`'s prefix up to and including the finalized
    /// message or config entry, whichever comes later; empty when both lie
    /// below the lineage's start. The message is matched as a
    /// `(this_msg, parent_msg)` pair, last occurrence taken.
    ///
    /// An unknown parent (fresh state or checkpoint restore) matches
    /// nothing: every boundary move records the parent, so until one happens
    /// the boundary entry sits at-or-below the LIB and cannot appear in a
    /// lineage.
    fn finalized_prefix_ids(&self, lineage: &[InscriptionInfo]) -> HashSet<MsgId> {
        let message = lineage.iter().rposition(|i| {
            i.this_msg == self.finalized_msg
                && self
                    .finalized_parent_msg
                    .is_some_and(|parent| i.parent_msg == parent)
        });
        let config = lineage
            .iter()
            .rposition(|i| i.this_msg == self.finalized_config);
        message.max(config).map_or_else(HashSet::new, |pos| {
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
        if let Some((block, block_tx)) = self.block_txs.values().find_map(|block| {
            block
                .channel_txs
                .iter()
                .find(|tx| tx.tx_hash() == Some(info.tx_hash))
                .map(|tx| (block, tx))
        }) {
            return match block_tx {
                BlockChannelTx::AtomicWithdraw(a) => {
                    Some(ChannelUpdateTx::AtomicWithdraw(a.clone()))
                }
                BlockChannelTx::PinDeposit(a) => Some(ChannelUpdateTx::PinDeposit(a.clone())),
                BlockChannelTx::Inscription(_) => Some(ChannelUpdateTx::Inscription(info.clone())),
                BlockChannelTx::Config(_) => block
                    .signed_txs
                    .iter()
                    .find(|tx| tx.hash() == info.tx_hash)
                    .map(|tx| ChannelUpdateTx::Config(tx.clone())),
                BlockChannelTx::Custom {
                    tx,
                    message_entries,
                    config_entries,
                } => (message_entries
                    .iter()
                    .any(|entry| !entry.payload.is_empty())
                    || !config_entries.is_empty())
                .then(|| ChannelUpdateTx::Custom(tx.clone())),
            };
        }
        // Not in any held block — the lineage bridged through a pending link.
        if let Some(other) = self.pending_other.get(&info.tx_hash) {
            let tx = other.signed_tx.clone();
            return Some(if other.pure_config {
                ChannelUpdateTx::Config(tx)
            } else {
                ChannelUpdateTx::Custom(tx)
            });
        }
        match self.pending.get(&info.tx_hash).map(|p| &p.bundle) {
            Some(PendingBundle::Withdraw { withdraws, outputs }) => {
                Some(ChannelUpdateTx::AtomicWithdraw(AtomicWithdrawInfo {
                    tx_hash: info.tx_hash,
                    inscription: info.clone(),
                    withdraws: withdraws.clone(),
                    outputs: outputs.clone(),
                }))
            }
            Some(PendingBundle::PinDeposit(consumed_notes)) => {
                Some(ChannelUpdateTx::PinDeposit(PinDepositInfo {
                    tx_hash: info.tx_hash,
                    inscription: info.clone(),
                    consumed_notes: consumed_notes.clone(),
                }))
            }
            Some(PendingBundle::Plain) | None => Some(ChannelUpdateTx::Inscription(info.clone())),
        }
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
        let blocks = tip
            .map(|tip| self.branch_blocks_above_lib(tip))
            .unwrap_or_default();
        self.wallet.view(blocks.iter())
    }

    /// Look up a tracked channel note by id (see
    /// [`ChannelWallet::find_note`](super::channel_wallet::ChannelWallet::find_note)).
    #[must_use]
    pub fn find_channel_note(&self, id: &NoteId) -> Option<&ChannelNote> {
        self.wallet.find_note(id)
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

    /// Keep a stored block's signed channel txs with the block.
    pub fn store_block_signed_txs(
        &mut self,
        block: HeaderId,
        txs: Vec<SignedOps<Unverified, StandardMode>>,
    ) {
        if !txs.is_empty() {
            self.block_txs.entry(block).or_default().signed_txs = txs;
        }
    }

    /// Signed channel txs on `tip`'s branch strictly above LIB (finalized never
    /// returns to pending) that pending lacks; empty in steady state.
    #[must_use]
    pub fn untracked_signed_txs_on_branch(
        &self,
        tip: HeaderId,
    ) -> Vec<SignedOps<Unverified, StandardMode>> {
        self.branch_blocks_above_lib(tip)
            .into_iter()
            .filter_map(|id| self.block_txs.get(&id))
            .flat_map(|block| block.signed_txs.iter())
            .filter(|tx| !self.is_tracked(&tx.hash()))
            .cloned()
            .collect()
    }

    /// The channel view at an L1 tip: mined inscriptions and configs on its
    /// branch plus the pending suffixes chaining from the mined tips of both
    /// lineages. Capture at the *old* tip before storing a new block.
    #[must_use]
    pub(crate) fn channel_lineage(&self, tip: HeaderId) -> Vec<InscriptionInfo> {
        let mut lineage = self.infos_on_branch(tip);
        let mut ids: HashSet<MsgId> = lineage.iter().map(|i| i.this_msg).collect();
        let message_suffix = self.collect_pending_suffix(self.channel_tip_at(tip));
        let config_suffix = self.collect_pending_config_suffix(self.config_tip_at(tip));
        for info in message_suffix.into_iter().chain(config_suffix) {
            if ids.insert(info.this_msg) {
                lineage.push(info);
            }
        }
        lineage
    }

    /// The pending chain off `from_msg`, parents first: plain entries by
    /// their own id, opaque txs by their inscriptions in op order.
    pub(crate) fn collect_pending_suffix(&self, from_msg: MsgId) -> Vec<InscriptionInfo> {
        let mut suffix = Vec::new();
        let mut visited: HashSet<MsgId> = HashSet::new();
        let mut current = from_msg;
        while visited.insert(current) {
            let Some(tx_hash) = self.pending_child(current) else {
                break;
            };
            if let Some(pending) = self.pending.get(&tx_hash) {
                suffix.push(pending.info());
                current = pending.this_msg;
            } else if let Some(other) = self.pending_other.get(&tx_hash) {
                suffix.extend(other.infos.iter().cloned());
                let Some(last_msg) = other.last_msg else {
                    break;
                };
                current = last_msg;
            } else {
                break;
            }
        }
        suffix
    }

    /// Config ids reachable from `config_tip` through pending opaque txs: a
    /// viable entry makes its own last config landable, so entries chained
    /// on it are viable too.
    fn landable_configs(&self, config_tip: MsgId) -> HashSet<MsgId> {
        let mut landable = HashSet::from([config_tip]);
        loop {
            let mut changed = false;
            for entry in self.pending_other.values() {
                let viable = entry
                    .config_parent
                    .is_some_and(|parent| landable.contains(&parent));
                if viable && let Some(last_config) = entry.last_config {
                    changed |= landable.insert(last_config);
                }
            }
            if !changed {
                return landable;
            }
        }
    }

    /// Pending config entries chaining from `config_tip`, in submission order.
    fn collect_pending_config_suffix(&self, config_tip: MsgId) -> Vec<InscriptionInfo> {
        let landable = self.landable_configs(config_tip);
        let mut entries: Vec<&PendingOtherTx> = self
            .pending_other
            .values()
            .filter(|entry| {
                entry
                    .config_parent
                    .is_some_and(|parent| landable.contains(&parent))
            })
            .collect();
        entries.sort_unstable_by_key(|entry| entry.seq);
        entries
            .into_iter()
            .flat_map(|entry| entry.config_infos.iter().cloned())
            .collect()
    }

    /// All tip-advancing entries on a branch back to LIB, oldest first:
    /// message entries and config entries, in op order.
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
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{Op, ops::channel::inscribe::InscriptionOp, transactions::Ops};
    use lb_key_management_system_service::keys::Ed25519PublicKey;

    use super::*;
    use crate::test_support::header_id;

    fn make_dummy_tx(data: u8) -> SignedOps<Unverified, StandardMode> {
        make_dummy_tx_on(MsgId::root(), data)
    }

    fn make_dummy_tx_on(parent: MsgId, data: u8) -> SignedOps<Unverified, StandardMode> {
        let mantle_tx = Ops::from([Op::ChannelInscribe(InscriptionOp {
            channel_id: [0u8; 32].into(),
            inscription: [data].into(),
            parent,
            signer: UnverifiedEd25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
        })]);
        SignedOps::from_ops_with_sample_proofs(mantle_tx)
    }

    fn dummy_tx_msg(tx: &SignedOps<Unverified, StandardMode>) -> MsgId {
        channel_inscriptions(tx, ChannelId::from([0u8; 32]))[0].this_msg
    }

    #[test]
    fn submit_and_query_pending() {
        let genesis = header_id(0);
        let mut state = TxState::new(genesis, MsgId::root());
        let tx = make_dummy_tx(1);

        state.submit_other(tx, ChannelId::from([0u8; 32])).unwrap();
        assert_eq!(state.unfinalized_count(), 1);
    }

    #[test]
    fn block_includes_tx() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.hash();
        state.submit_other(tx, ChannelId::from([0u8; 32])).unwrap();

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
        let hash = tx.hash();
        state.submit_other(tx, ChannelId::from([0u8; 32])).unwrap();

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
        let tx2 = make_dummy_tx_on(dummy_tx_msg(&tx1), 2);
        let hash1 = tx1.hash();
        let hash2 = tx2.hash();

        state.submit_other(tx1, ChannelId::from([0u8; 32])).unwrap();
        state.submit_other(tx2, ChannelId::from([0u8; 32])).unwrap();

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
        let hash = tx.hash();
        state.submit_other(tx, ChannelId::from([0u8; 32])).unwrap();

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
            note_id: NoteId::from(lb_groth16::Fr::from(seed)),
            value,
            pk: lb_groth16::Fr::from(seed).into(),
            slot: lb_common_http_client::Slot::from(1),
        })
    }

    fn wallet_note_id(seed: u64) -> NoteId {
        NoteId::from(lb_groth16::Fr::from(seed))
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
    fn bundle_tx(parent: MsgId, data: u8) -> (SignedOps<Unverified, StandardMode>, MsgId, MsgId) {
        use lb_core::mantle::{
            channel::{SlotTimeframe, SlotTimeout},
            ops::channel::{VerifiedChannelKeys, config::ChannelConfigOp},
        };
        let inscribe = InscriptionOp {
            channel_id: [0u8; 32].into(),
            inscription: [data].into(),
            parent,
            signer: UnverifiedEd25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
        };
        let config = ChannelConfigOp {
            channel: [0u8; 32].into(),
            parent: MsgId::root(),
            keys: VerifiedChannelKeys::try_from(vec![
                Ed25519PublicKey::from_bytes(&[1u8; 32]).unwrap(),
            ])
            .unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let inscribe_msg = inscribe.id();
        let config_msg = config.id();
        let ops = Ops::from([Op::ChannelInscribe(inscribe), Op::ChannelConfig(config)]);
        let tx = SignedOps::from_ops_with_sample_proofs(ops);
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
        let derived = state.submit_other(bundle, channel_id).unwrap();
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
            hashes.push(bundle.hash());
            state.submit_other(bundle, channel_id).unwrap();
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

        state
            .submit_inscription(make_dummy_tx(1), MsgId::root(), msg_id(10), [1].into())
            .unwrap();
        let (bundle, inscribe_msg, _config_msg) = bundle_tx(msg_id(10), 2);
        state.submit_other(bundle, channel_id).unwrap();
        state
            .submit_inscription(make_dummy_tx(3), inscribe_msg, msg_id(30), [3].into())
            .unwrap();

        assert_eq!(state.publish_parent(tip), msg_id(30));
    }

    #[test]
    fn opaque_tx_on_a_taken_parent_is_refused() {
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(tip, genesis, genesis, vec![], vec![], Vec::new());

        let first = make_dummy_tx(1);
        let first_hash = first.hash();
        state
            .submit_inscription(first, MsgId::root(), msg_id(10), [1].into())
            .unwrap();
        let (bundle, _i, _c) = bundle_tx(MsgId::root(), 2);
        assert_eq!(
            state.submit_other(bundle, channel_id),
            Err(ParentTaken {
                parent: MsgId::root(),
                by: first_hash
            })
        );
        assert_eq!(state.publish_parent(tip), msg_id(10));
    }

    /// A pure config cut (no inscription in the tx) is NOT chained: it lands
    /// whenever it lands and orphans what it cut off — publishes keep
    /// chaining on the existing pending chain meanwhile.
    #[test]
    fn publish_parent_ignores_pending_pure_config_cut() {
        use lb_core::mantle::{
            channel::{SlotTimeframe, SlotTimeout},
            ops::channel::{VerifiedChannelKeys, config::ChannelConfigOp},
        };
        let genesis = header_id(0);
        let tip = header_id(1);
        let channel_id = ChannelId::from([0u8; 32]);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(tip, genesis, genesis, vec![], vec![], Vec::new());

        state
            .submit_inscription(make_dummy_tx(1), MsgId::root(), msg_id(10), [1].into())
            .unwrap();

        let config = ChannelConfigOp {
            channel: [0u8; 32].into(),
            parent: MsgId::root(),
            keys: VerifiedChannelKeys::try_from(vec![
                Ed25519PublicKey::from_bytes(&[1u8; 32]).unwrap(),
            ])
            .unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let ops = Ops::from([Op::ChannelConfig(config)]);
        let config_tx = SignedOps::from_ops_with_sample_proofs(ops);
        state.submit_other(config_tx, channel_id).unwrap();

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

    /// A throwaway inscription author for test fixtures (never asserted on).
    fn test_signer() -> UnverifiedEd25519PublicKey {
        UnverifiedEd25519PublicKey::from_bytes(&[0u8; 32]).unwrap()
    }

    /// Submit a fake pending inscription with lineage metadata.
    fn submit_fake_inscription(
        state: &mut TxState,
        data: u8,
        parent_msg: MsgId,
        this_msg: MsgId,
    ) -> TxHash {
        let tx = make_dummy_tx(data);
        let hash = tx.hash();
        state
            .submit_inscription(tx, parent_msg, this_msg, [data].into())
            .unwrap();
        hash
    }

    /// Build a pure `[config]` tx for the zero channel; `data` varies the
    /// payload so ids differ.
    fn config_tx(parent: MsgId, data: u32) -> (SignedOps<Unverified, StandardMode>, MsgId) {
        use lb_core::mantle::{
            channel::{SlotTimeframe, SlotTimeout},
            ops::channel::{VerifiedChannelKeys, config::ChannelConfigOp},
        };
        let config = ChannelConfigOp {
            channel: [0u8; 32].into(),
            parent,
            keys: VerifiedChannelKeys::try_from(vec![
                Ed25519PublicKey::from_bytes(&[1u8; 32]).unwrap(),
            ])
            .unwrap(),
            posting_timeframe: SlotTimeframe::from(data),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let config_msg = config.id();
        let tx = SignedOps::from_ops_with_sample_proofs(Ops::from([Op::ChannelConfig(config)]));
        (tx, config_msg)
    }

    /// Wrap a pure config tx as it is classified when mined: a clean
    /// [`BlockChannelTx::Config`] on the config lineage (the mixed/unknown
    /// `Custom { config_entries }` shape is exercised separately in
    /// `mixed_config_tx_is_custom_but_advances_the_config_tip`).
    fn config_block_tx(
        tx: &SignedOps<Unverified, StandardMode>,
        this_msg: MsgId,
        parent: MsgId,
    ) -> BlockChannelTx {
        BlockChannelTx::Config(InscriptionInfo {
            tx_hash: tx.hash(),
            parent_msg: parent,
            this_msg,
            payload: [].into(),
            signer: None,
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
        let stale_hash = stale.hash();
        state.submit_other(stale, channel_id).unwrap();

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
        assert_eq!(shed[0].hash(), stale_hash);
        assert!(!state.pending_other_contains(&stale_hash));
    }

    #[test]
    fn finalized_prefix_masks_every_config_in_the_lib_block() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());
        let (c1_tx, c1) = config_tx(MsgId::root(), 1);
        let (c2_tx, c2) = config_tx(c1, 2);
        state.process_block(
            b1,
            genesis,
            genesis,
            vec![],
            vec![
                config_block_tx(&c1_tx, c1, MsgId::root()),
                config_block_tx(&c2_tx, c2, c1),
            ],
            Vec::new(),
        );
        state.process_block(b2, b1, b1, vec![], vec![], Vec::new());
        assert_eq!(state.finalized_config(), c2);
        assert!(
            state
                .detect_channel_update(&[], b2, &HashSet::new())
                .is_none(),
            "finalized configs are not reported"
        );
    }

    #[test]
    fn finalized_configs_are_not_reported_orphaned_on_lib_advance() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let b3 = header_id(3);
        let mut state = TxState::new(genesis, MsgId::root());
        let (c1_tx, c1) = config_tx(MsgId::root(), 1);
        let (c2_tx, c2) = config_tx(c1, 2);
        state.process_block(
            b1,
            genesis,
            genesis,
            vec![],
            vec![config_block_tx(&c1_tx, c1, MsgId::root())],
            Vec::new(),
        );
        state.process_block(
            b2,
            b1,
            genesis,
            vec![],
            vec![config_block_tx(&c2_tx, c2, c1)],
            Vec::new(),
        );
        let old_lineage = state.channel_lineage(b2);
        state.process_block(b3, b2, b2, vec![], vec![], Vec::new());
        assert!(
            state
                .detect_channel_update(&old_lineage, b3, &HashSet::new())
                .is_none()
        );
    }

    #[test]
    fn custom_tx_advancing_only_the_config_lineage_is_reported_as_custom() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        let (tx, _) = config_tx(MsgId::root(), 1);
        let config_entry = |parent: MsgId, this_msg: MsgId| InscriptionInfo {
            tx_hash: tx.hash(),
            parent_msg: parent,
            this_msg,
            payload: [].into(),
            signer: None,
        };
        let custom = BlockChannelTx::Custom {
            tx: tx.clone(),
            message_entries: Vec::new(),
            config_entries: vec![
                config_entry(MsgId::root(), msg_id(1)),
                config_entry(msg_id(1), msg_id(2)),
            ],
        };
        state.process_block(b1, genesis, genesis, vec![], vec![custom], Vec::new());
        let update = state
            .detect_channel_update(&[], b1, &HashSet::new())
            .expect("configs enter the view");
        assert!(
            matches!(update.adopted.as_slice(), [ChannelUpdateTx::Custom(t)] if t.hash() == tx.hash())
        );
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

    /// Restoring a finalized config tip must also seed `observed_config_tip`,
    /// so the config-driven shed does not treat the already-finalized
    /// config as a fresh landing and orphan the pending tail on the first
    /// block after a resume/backfill.
    #[test]
    fn restored_finalized_config_does_not_spuriously_shed_pending() {
        let genesis = header_id(0);
        let mut state = TxState::new(genesis, MsgId::root());

        // A resume/backfill hands us the finalized config tip...
        state.set_finalized_config(msg_id(7));
        // ...and a pending inscription is in flight (observed or our own).
        submit_fake_inscription(&mut state, 1, MsgId::root(), msg_id(1));

        // The first shed at a tip resolving to that already-finalized config
        // must be a no-op — without seeding `observed_config_tip` it would fire
        // (`config_tip != root`) and orphan the pending tail.
        assert!(
            state
                .shed_pending_inscriptions_on_config(genesis)
                .is_empty(),
            "an already-finalized config must not orphan pending on resume"
        );
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
        let stale_hash = stale.hash();
        let (on_tip, _) = config_tx(c2_msg, 4);
        let on_tip_hash = on_tip.hash();
        state.submit_other(stale, channel_id).unwrap();
        state.submit_other(on_tip, channel_id).unwrap();

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
        assert_eq!(shed[0].hash(), stale_hash);
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
        let config_hash = config.hash();
        // Build the block entry before `submit_other` moves the tx.
        let block_tx = config_block_tx(&config, config_msg, MsgId::root());
        state.submit_other(config, channel_id).unwrap();

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
        let c_hash = c_config.hash();
        state.submit_other(c_config, channel_id).unwrap();

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
            signer: Some(test_signer()),
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
            tx_hash: make_dummy_tx(1).hash(),
            parent_msg: MsgId::root(),
            this_msg: msg_id(1),
            payload: [1].into(),
            signer: Some(test_signer()),
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
            state
                .detect_channel_update(&old_lineage, b2, &HashSet::new())
                .is_none(),
            "a config-only block does not change the message lineage"
        );
        assert_eq!(state.channel_tip_at(b2), msg_id(1));
        assert!(state.pending_txs(b2).iter().any(|(h, _)| *h == p_hash));
        assert!(state.shed_off_branch_pending(b2).is_empty());
    }

    #[test]
    fn extension_with_competing_inscription_orphans_displaced_local_pending() {
        // Scenario: local pending b1→b2→b3 from root, so the view is
        // b1,b2,b3. Competing c1 lands on chain consuming root as parent:
        // the view becomes c1, so the diff reports b1,b2,b3 orphaned and c1
        // adopted (the shed reports the same entries; the actor dedups).
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
        let c1_tx_hash = c1_tx.hash();
        let c1_inscription = InscriptionInfo {
            tx_hash: c1_tx_hash,
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: [99].into(),
            signer: Some(test_signer()),
        };
        // Mirror the observed inscription into pending before the safe-set
        // build, as `handle_block_event` does — the pending set reflects the
        // channel view, so c1 is retried too if it later reorgs out.
        state.observe_channel_inscription(
            c1_tx,
            MsgId::root(),
            c1_msg,
            [99].into(),
            PendingBundle::Plain,
        );
        state.process_block(
            block2,
            block1,
            genesis,
            vec![c1_tx_hash],
            vec![BlockChannelTx::Inscription(c1_inscription)],
            Vec::new(),
        );

        let update = state
            .detect_channel_update(&old_lineage, block2, &HashSet::new())
            .expect("should detect channel update");

        let orphaned: Vec<MsgId> = update
            .orphaned
            .iter()
            .filter_map(|t| t.inscription().map(|i| i.this_msg))
            .collect();
        assert_eq!(
            orphaned,
            vec![b1_msg, b2_msg, b3_msg],
            "displaced pending suffix"
        );
        assert_eq!(update.adopted.len(), 1);
        assert_eq!(update.adopted[0].inscription().unwrap().this_msg, c1_msg);
        // The mined c1 took root's position: b1 and its chain are displaced
        // and come out of the shed pass parents first; c1 is tracked
        // (already `posted`, excluded from re-posting while its block is
        // on-branch via the safe set).
        assert_eq!(state.pending.len(), 1);
        let shed: Vec<MsgId> = state
            .shed_off_branch_pending(block2)
            .iter()
            .map(|t| t.inscription().this_msg)
            .collect();
        assert_eq!(shed, vec![b1_msg, b2_msg, b3_msg]);
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
    fn second_pending_child_of_a_parent_is_refused() {
        let genesis = header_id(0);
        let block1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(block1, genesis, genesis, vec![], vec![], Vec::new());

        let b1_msg = msg_id(10);
        let b1_hash = submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        let refused =
            state.submit_inscription(make_dummy_tx(4), MsgId::root(), msg_id(30), [4].into());
        assert_eq!(
            refused,
            Err(ParentTaken {
                parent: MsgId::root(),
                by: b1_hash
            })
        );
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.publish_parent(block1), b1_msg);
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
            tx_hash: make_dummy_tx(99).hash(),
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: [99].into(),
            signer: Some(test_signer()),
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
        let tx2 = make_dummy_tx_on(dummy_tx_msg(&tx1), 2);
        let hash1 = tx1.hash();
        let hash2 = tx2.hash();

        state.submit_other(tx1, ChannelId::from([0u8; 32])).unwrap();
        state.submit_other(tx2, ChannelId::from([0u8; 32])).unwrap();

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
