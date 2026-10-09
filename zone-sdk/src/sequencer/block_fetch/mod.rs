use std::collections::{HashMap, HashSet};

use lb_common_http_client::{ApiBlock, ProcessedBlockEvent, Slot};
use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps,
        ledger::{
            Inputs, Outputs,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::{
            OpId as _, OpRef,
            channel::{
                ChannelId, MsgId, channel_transfer::ChannelTransferOp, inscribe::Inscription,
            },
        },
        traits::Hashable as _,
        transactions::{
            hash::TxHash,
            states::{Unverified, VerificationState},
        },
    },
};
use tracing::{debug, error};

use super::{
    TARGET,
    channel_wallet::{NoteOp, note_ops_from_txs},
    state::{BlockChannelTx, ChannelUpdateInfo, PendingBundle, TxState},
    types::{
        AtomicWithdrawInfo, ChannelTransferInfo, ChannelUpdateTx, DepositInfo, Error, FinalizedOp,
        FinalizedTx, InscriptionInfo, PendingTx, PinDepositInfo, WithdrawInfo,
    },
};
use crate::{
    adapter,
    adapter::{DepositEvents, DepositOpKey, build_deposit_events},
};

/// Result of processing a block event.
pub(super) struct BlockEventResult {
    /// Finalized channel txs in tx/op execution order across blocks. Each
    /// [`FinalizedTx`] groups all channel-relevant ops from a single Mantle
    /// tx — inscriptions (ours or others'), deposits (with `amount` from the
    /// chain events API) and withdraws (standalone or part of an atomic
    /// inscription+withdraw bundle).
    pub(super) finalized_items: Vec<FinalizedTx>,
    pub(super) channel_update: Option<ChannelUpdateInfo>,
    /// The view above LIB minus `adopted`, in lineage order.
    pub(super) common_prefix: Vec<ChannelUpdateTx>,
    /// Channel deposits observed in the blocks this event covers (canonical
    /// backfill first, then the live block), in block and op order. Surfaced
    /// non-finalized on `Event::BlocksProcessed` so a consumer can pin a
    /// deposit without waiting for finalization.
    pub(super) deposits: Vec<DepositInfo>,
}

struct PreparedBlockEvent<'a> {
    block: &'a ApiBlock,
    tip: HeaderId,
    lib: HeaderId,
    lib_slot: Slot,
    lib_advanced: bool,
    finalized: Vec<PreparedFinalizedBlock>,
    /// Each canonical-backfill block paired with its (pre-computed, pure)
    /// channel-note ops so the apply phase stays free of node fetches.
    canonical_backfill: Vec<(ApiBlock, Vec<NoteOp>)>,
    our_txs: Vec<TxHash>,
    channel_txs: Vec<BlockChannelTx>,
    /// Channel-note ops of the live block, computed in the prepare phase.
    note_ops: Vec<NoteOp>,
    deposits: Vec<DepositInfo>,
}

/// Process a block event. Returns finalized tx hashes and optional channel
/// update.
///
/// Returns [`Err`] if the LIB-range backfill (blocks or deposit events) fails
/// for this event. On error, `state`, `current_tip`, and `lib_slot` are left
/// untouched so the caller can drop the block stream and have the reconnect
/// path retry this same event.
pub(super) async fn handle_block_event<Node>(
    event: &ProcessedBlockEvent,
    state: &mut Option<TxState>,
    current_tip: &mut Option<HeaderId>,
    lib_slot: &mut Slot,
    channel_id: ChannelId,
    node: &Node,
) -> Result<BlockEventResult, Error>
where
    Node: adapter::Node + Sync,
{
    let prepared = prepare_block_event(event, state.as_ref(), *lib_slot, channel_id, node).await?;

    Ok(apply_prepared_block_event(
        prepared,
        state,
        current_tip,
        lib_slot,
        channel_id,
    ))
}

async fn prepare_block_event<'a, Node>(
    event: &'a ProcessedBlockEvent,
    state: Option<&TxState>,
    lib_slot: Slot,
    channel_id: ChannelId,
    node: &Node,
) -> Result<PreparedBlockEvent<'a>, Error>
where
    Node: adapter::Node + Sync,
{
    let state_lib = state.map_or(event.lib, TxState::lib);
    let lib_advanced = event.lib != state_lib;
    let finalized = if lib_advanced {
        let from: u64 = lib_slot.into();
        let to: u64 = event.lib_slot.into();
        if from < to {
            prepare_finalized_blocks(from + 1, to, channel_id, node, state).await?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let finalized_block_ids: HashSet<HeaderId> =
        finalized.iter().map(|block| block.block_id).collect();
    let parent_id = event.block.header.parent_block;
    let parent_known = block_is_known(state, &finalized_block_ids, state_lib, parent_id);
    let (canonical_backfill, mut deposits) = if parent_known {
        (Vec::new(), Vec::new())
    } else {
        let blocks =
            walk_back_to_known(state, &finalized_block_ids, state_lib, parent_id, node).await?;
        prepare_backfill_blocks(blocks, channel_id, node).await?
    };

    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| touches_channel_tip(tx, channel_id))
        .map(|tx| tx.op_refs().hash())
        .collect();
    let channel_txs = classify_channel_txs(&event.block.transactions, channel_id);

    // Deposit events + wallet note ops for the live block: fetched here (the
    // prepare phase) so the apply phase mutates state without any `.await`.
    let deposit_events = fetch_block_deposit_events(
        node,
        event.block.header.id,
        &event.block.transactions,
        channel_id,
    )
    .await?;
    let note_ops = note_ops_from_txs(
        &event.block.transactions,
        channel_id,
        &deposit_events,
        event.block.header.slot,
    );
    deposits.extend(block_channel_deposits(
        &event.block.transactions,
        channel_id,
        event.block.header.slot,
        &deposit_events,
    ));

    Ok(PreparedBlockEvent {
        block: &event.block,
        tip: event.tip,
        lib: event.lib,
        lib_slot: event.lib_slot,
        lib_advanced,
        finalized,
        canonical_backfill,
        our_txs,
        channel_txs,
        note_ops,
        deposits,
    })
}

/// The channel deposits in a single block, in on-chain op order — the
/// non-finalized counterpart of the finalized deposit stream, surfaced so a
/// consumer can pin a deposit before it finalizes.
fn block_channel_deposits(
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
    l1_slot: Slot,
    deposit_events: &DepositEvents,
) -> Vec<DepositInfo> {
    extract_finalized_items(transactions, channel_id, l1_slot, deposit_events)
        .into_iter()
        .flat_map(|item| item.ops)
        .filter_map(|op| match op {
            FinalizedOp::Deposit(d) => Some(d),
            _ => None,
        })
        .collect()
}

fn apply_prepared_block_event(
    prepared: PreparedBlockEvent<'_>,
    state: &mut Option<TxState>,
    current_tip: &mut Option<HeaderId>,
    lib_slot: &mut Slot,
    channel_id: ChannelId,
) -> BlockEventResult {
    let PreparedBlockEvent {
        block,
        tip,
        lib,
        lib_slot: next_lib_slot,
        lib_advanced,
        finalized,
        canonical_backfill,
        our_txs,
        mut channel_txs,
        note_ops,
        deposits,
    } = prepared;

    if state.is_none() {
        *state = Some(TxState::new(lib, MsgId::root()));
    }
    let s = state.as_mut().expect("state initialized above");

    let old_tip = *current_tip;

    // Snapshot which txs were tracked BEFORE this event mutates state: the
    // extension-case `adopted` filter below distinguishes entries the
    // sequencer already knew about (its own publishes and previously
    // observed ones) from genuinely new network entries.
    let tracked_before = s.tracked_tx_hashes();
    let tracked_msgs_before = s.tracked_msg_ids();

    // Install finalized history first. It is not mirrored into pending: the
    // matching local entries are removed below using the returned hashes.
    let finalized_batch = apply_finalized_blocks(s, finalized);
    if lib_advanced {
        *lib_slot = next_lib_slot;
    }

    // Capture the old-tip lineage before canonical backfill adds blocks: the
    // lineage walk bridges through held blocks, and whatever is already in
    // the store lands on the "before" side of the update diff.
    let old_lineage = old_tip.map(|old| s.channel_lineage(old));

    let current_lib = s.lib();
    for (block, note_ops) in canonical_backfill {
        apply_backfilled_block(s, &block, channel_id, current_lib, note_ops);
    }

    // Re-type unsound pin-deposits to `Custom` before they are stored
    // or mirrored, so both the `adopted` surface and the pending set agree.
    demote_non_identity_pin_deposits(&mut channel_txs, &block.transactions, channel_id, s);

    // Pending = canonical channel txs above LIB + our own unmined publishes:
    // mirror only the canonical tip; a fork's content joins from the store if
    // its branch wins.
    if block.header.id == tip {
        mirror_channel_txs(s, &channel_txs, &block.transactions, channel_id);
    }
    s.store_block_signed_txs(
        block.header.id,
        mirrorable_txs(&channel_txs, &block.transactions),
    );

    // Process the actual event block
    s.process_block(
        block.header.id,
        block.header.parent_block,
        lib,
        our_txs,
        channel_txs,
        note_ops,
    );

    // Remove our pending txs that were finalized in the backfilled LIB blocks,
    // by tx hash and by message id. `finalized_items` already carries the
    // typed payloads (built before pending was mutated).
    let finalized_now: HashSet<MsgId> = finalized_batch
        .items
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .filter_map(|op| match op {
            FinalizedOp::Inscription(i) | FinalizedOp::Config(i) => Some(i.this_msg),
            _ => None,
        })
        .collect();
    for tx_hash in &finalized_batch.our_tx_hashes {
        s.remove_pending(tx_hash);
    }
    s.take_landed(&finalized_now);

    *current_tip = Some(tip);

    mirror_branch_from_store(s, tip, channel_id);

    // Detect channel changes by diffing the channel view on every block. On
    // the first event there is no old tip: what restored pending chains on
    // was the view before the restart, and the rest of the channel is new
    // (a clean start on an existing channel).
    let old_lineage =
        old_lineage.unwrap_or_else(|| s.lineage_under(tip, &tracked_before, &tracked_msgs_before));
    let channel_update = s.detect_channel_update(&old_lineage, tip, &finalized_now);

    // On a pure extension (nothing orphaned — including the first event,
    // whose `orphaned` is empty by construction), report only entries the
    // sequencer didn't already track: its own publishes land on the channel
    // through its own action and must not echo back. On a branch change the
    // full delta flows through unfiltered.
    let channel_update = channel_update.map(|mut update| {
        if update.orphaned.is_empty() {
            update
                .adopted
                .retain(|tx| !tracked_before.contains(&tx.tx_hash()));
        }
        update
    });

    // LIB to the fork point, then the pending tail still chaining on it: what
    // the view had before this event. A pending entry survives a branch
    // change only by chaining on the fork point, since anything adopted
    // above it takes its slot and sheds it.
    // Matched by message or config id; by tx hash for an entry carrying
    // neither.
    let old_txs: HashSet<TxHash> = old_lineage.iter().map(|info| info.tx_hash).collect();
    let old_msgs: HashSet<MsgId> = old_lineage.iter().map(|info| info.this_msg).collect();
    let common_prefix = s
        .channel_view_txs(tip, &finalized_now)
        .into_iter()
        .filter(|tx| {
            let mut ids = update_tx_msg_ids(tx, channel_id).peekable();
            if ids.peek().is_some() {
                ids.any(|id| old_msgs.contains(&id))
            } else {
                old_txs.contains(&tx.tx_hash())
            }
        })
        .collect();

    BlockEventResult {
        finalized_items: finalized_batch.items,
        channel_update,
        common_prefix,
        deposits,
    }
}

/// (Re-)mirror stored blocks now on the canonical path: arrived as a fork,
/// or shed on an earlier switch.
fn mirror_branch_from_store(s: &mut TxState, tip: HeaderId, channel_id: ChannelId) {
    let untracked = s.untracked_signed_txs_on_branch(tip);
    if untracked.is_empty() {
        return;
    }
    let mut classified = classify_channel_txs(&untracked, channel_id);
    demote_non_identity_pin_deposits(&mut classified, &untracked, channel_id, s);
    mirror_channel_txs(s, &classified, &untracked, channel_id);
}

/// Mirror a block's channel txs into the pending set (insert-if-absent) so a
/// later retry re-posts the original bytes.
fn mirror_channel_txs(
    state: &mut TxState,
    classified: &[BlockChannelTx],
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
) {
    let by_hash: HashMap<TxHash, &SignedOps<Unverified, StandardMode>> =
        transactions.iter().map(|tx| (tx.hash(), tx)).collect();
    for block_tx in classified {
        let (info, bundle) = match block_tx {
            BlockChannelTx::Inscription(i) => (i, PendingBundle::Plain),
            BlockChannelTx::AtomicWithdraw(a) => (
                &a.inscription,
                PendingBundle::Withdraw {
                    withdraws: a.withdraws.clone(),
                    outputs: a.outputs.clone(),
                },
            ),
            BlockChannelTx::PinDeposit(a) => (
                &a.inscription,
                PendingBundle::PinDeposit(a.consumed_notes.clone()),
            ),
            BlockChannelTx::Config(_) | BlockChannelTx::Custom { .. } => {
                let tx_hash = block_tx
                    .tx_hash()
                    .expect("custom and config shapes carry their tx hash");
                let tx = by_hash
                    .get(&tx_hash)
                    .expect("classified entries come from these transactions");
                state.observe_other_tx((*tx).clone(), channel_id);
                continue;
            }
        };
        let tx = by_hash
            .get(&info.tx_hash)
            .expect("classified entries come from these transactions");
        state.observe_channel_inscription(
            (*tx).clone(),
            info.parent_msg,
            info.this_msg,
            info.payload.clone(),
            bundle,
        );
    }
}

/// Re-type any [`BlockChannelTx::PinDeposit`] whose transfer is
/// not a 1:1 identity re-creation of its consumed notes as `Custom` — a split,
/// merge, re-value or re-key would make `consumed_notes` misdescribe the
/// deposit. Runs before the classification is stored or mirrored.
fn demote_non_identity_pin_deposits(
    channel_txs: &mut [BlockChannelTx],
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
    state: &TxState,
) {
    let by_hash: HashMap<TxHash, &SignedOps<Unverified, StandardMode>> =
        transactions.iter().map(|tx| (tx.hash(), tx)).collect();
    for block_tx in channel_txs.iter_mut() {
        let BlockChannelTx::PinDeposit(a) = block_tx else {
            continue;
        };
        let tx = by_hash
            .get(&a.tx_hash)
            .expect("classified entries come from these transactions");
        if !is_identity_deposit_transfer(state, tx, channel_id) {
            *block_tx = BlockChannelTx::Custom {
                tx: (*tx).clone(),
                message_entries: vec![a.inscription.clone()],
                config_entries: Vec::new(),
            };
        }
    }
}

/// Whether the bundle's `ChannelTransfer` re-creates its inputs unchanged —
/// same count and `(value, key)` multiset. An input note we do not track (so
/// cannot compare) is treated as non-identity.
fn is_identity_deposit_transfer(
    state: &TxState,
    tx: &SignedOps<Unverified, StandardMode>,
    channel_id: ChannelId,
) -> bool {
    let Some(transfer) = channel_transfers(tx, channel_id).next() else {
        return false;
    };
    let mut outputs: Vec<_> = transfer
        .utxos()
        .map(|u| (u.note.value, u.note.pk))
        .collect();
    if transfer.inputs.len() != outputs.len() {
        return false;
    }
    for id in transfer.inputs.iter() {
        let Some(note) = state.find_channel_note(id) else {
            return false;
        };
        match outputs.iter().position(|out| *out == (note.value, note.pk)) {
            Some(pos) => {
                outputs.swap_remove(pos);
            }
            None => return false,
        }
    }
    outputs.is_empty()
}

/// A tx's `ChannelTransfer` ops for `channel_id`, in op order.
pub(super) fn channel_transfers(
    tx: &SignedOps<Unverified, StandardMode>,
    channel_id: ChannelId,
) -> impl Iterator<Item = &ChannelTransferOp> {
    tx.op_refs_iter().filter_map(move |op| match op {
        OpRef::ChannelTransfer(t) if t.channel_id == channel_id => Some(t),
        _ => None,
    })
}

/// Extract a tx's channel inscriptions, in op order. `ChannelConfig` ops are
/// not part of the message lineage and yield no entries.
#[must_use]
pub fn channel_inscriptions(
    tx: &SignedOps<Unverified, StandardMode>,
    channel_id: ChannelId,
) -> Vec<InscriptionInfo> {
    let tx_hash = tx.op_refs().hash();
    let mut entries: Vec<InscriptionInfo> = Vec::new();
    for op in tx.op_refs() {
        if let OpRef::ChannelInscribe(inscribe) = op
            && inscribe.channel_id == channel_id
        {
            entries.push(InscriptionInfo {
                tx_hash,
                parent_msg: inscribe.parent,
                this_msg: inscribe.id(),
                payload: inscribe.inscription.clone(),
                signer: Some(inscribe.signer),
            });
        }
    }
    entries
}

/// The message and config ids an update entry carries for `channel_id`.
fn update_tx_msg_ids(
    tx: &ChannelUpdateTx,
    channel_id: ChannelId,
) -> impl Iterator<Item = MsgId> + '_ {
    let typed = tx.inscription().map(|info| info.this_msg);
    let opaque = match tx {
        ChannelUpdateTx::Config(signed) | ChannelUpdateTx::Custom(signed) => {
            let mut ids: Vec<MsgId> = channel_inscriptions(signed, channel_id)
                .iter()
                .map(|info| info.this_msg)
                .collect();
            ids.extend(
                channel_configs(signed, channel_id)
                    .iter()
                    .map(|info| info.this_msg),
            );
            ids
        }
        _ => Vec::new(),
    };
    typed.into_iter().chain(opaque)
}

/// A tx's config-lineage entries for `channel_id`, in op order.
pub fn channel_configs(
    tx: &SignedOps<Unverified, StandardMode>,
    channel_id: ChannelId,
) -> Vec<InscriptionInfo> {
    let tx_hash = tx.op_refs().hash();
    tx.op_refs_iter()
        .filter_map(|op| match op {
            OpRef::ChannelConfig(config) if config.channel == channel_id => Some(InscriptionInfo {
                tx_hash,
                parent_msg: config.parent,
                this_msg: config.id(),
                payload: Inscription::new_unchecked(Vec::new()),
                signer: None,
            }),
            _ => None,
        })
        .collect()
}

/// Convert a shed pending entry into a [`ChannelUpdateTx`] for surfacing to
/// the consumer.
pub(super) fn orphan_from_shed(entry: PendingTx) -> ChannelUpdateTx {
    let info = entry.inscription();
    debug!(
        target: TARGET,
        "  orphaned: payload={:?}, tx={}, msg_id={}",
        String::from_utf8_lossy(&info.payload),
        hex::encode(info.tx_hash.0),
        hex::encode(info.this_msg.as_ref()),
    );
    match entry {
        PendingTx::Inscription(i) => ChannelUpdateTx::Inscription(i),
        PendingTx::AtomicWithdraw(a) => ChannelUpdateTx::AtomicWithdraw(a),
        PendingTx::PinDeposit(a) => ChannelUpdateTx::PinDeposit(a),
    }
}

/// Result of fetching and processing a slot range.
pub(super) struct FetchedBatch {
    /// Tx hashes of txs that match our channel (any op). Used internally to
    /// clean up our pending set.
    pub(super) our_tx_hashes: Vec<TxHash>,
    /// User-facing finalized txs, one entry per channel-relevant Mantle tx,
    /// in block then tx order across the range. Each entry carries its ops
    /// in on-chain execution order.
    pub(super) items: Vec<FinalizedTx>,
}

struct PreparedFinalizedBlock {
    block_id: HeaderId,
    parent_id: HeaderId,
    our_txs: Vec<TxHash>,
    channel_txs: Vec<BlockChannelTx>,
    items: Vec<FinalizedTx>,
    /// Wallet note ops for this finalized block, applied straight to the
    /// finalized base (never the per-block overlay).
    note_ops: Vec<NoteOp>,
}

async fn prepare_finalized_blocks<Node>(
    from_slot: u64,
    to_slot: u64,
    channel_id: ChannelId,
    node: &Node,
    state: Option<&TxState>,
) -> Result<Vec<PreparedFinalizedBlock>, Error>
where
    Node: adapter::Node + Sync,
{
    let blocks = node
        .immutable_blocks(Slot::from(from_slot), Slot::from(to_slot))
        .await
        .map_err(|e| {
            error!(target: TARGET, ?from_slot, ?to_slot, ?e, "Failed to fetch immutable blocks");
            Error::Network(format!(
                "failed to fetch blocks (slots {from_slot}..{to_slot}): {e}"
            ))
        })?;

    let mut prepared = Vec::with_capacity(blocks.len());
    for block in blocks {
        let our_txs: Vec<TxHash> = block
            .transactions
            .iter()
            .filter(|tx| touches_channel_tip(tx, channel_id))
            .map(|tx| tx.op_refs().hash())
            .collect();

        let mut channel_txs = classify_channel_txs(&block.transactions, channel_id);
        // Below-LIB blocks feed the lineage walk too, so on first sync (empty
        // old lineage) they surface as `adopted` — demote here as well. A
        // same-batch deposit isn't applied yet, so its inscription can only
        // under-label to `Custom`, which is safe.
        if let Some(state) = state {
            demote_non_identity_pin_deposits(
                &mut channel_txs,
                &block.transactions,
                channel_id,
                state,
            );
        }

        // Fetch + validate deposit events for this block BEFORE mutating
        // state — on error we leave state untouched so the caller can retry.
        let deposit_events =
            fetch_block_deposit_events(node, block.header.id, &block.transactions, channel_id)
                .await?;
        let block_items = extract_finalized_items(
            &block.transactions,
            channel_id,
            block.header.slot,
            &deposit_events,
        );

        let note_ops = note_ops_from_txs(
            &block.transactions,
            channel_id,
            &deposit_events,
            block.header.slot,
        );
        prepared.push(PreparedFinalizedBlock {
            block_id: block.header.id,
            parent_id: block.header.parent_block,
            our_txs,
            channel_txs,
            items: block_items,
            note_ops,
        });
    }

    Ok(prepared)
}

fn apply_finalized_blocks(
    state: &mut TxState,
    blocks: Vec<PreparedFinalizedBlock>,
) -> FetchedBatch {
    let mut result = FetchedBatch {
        our_tx_hashes: Vec::new(),
        items: Vec::new(),
    };

    for block in blocks {
        result.our_tx_hashes.extend(block.our_txs.iter().copied());
        result.items.extend(block.items);

        // Immutable blocks: note ops go straight to the wallet's finalized
        // base, never through the per-block overlay.
        state.apply_finalized_note_ops(block.note_ops);

        state.process_block(
            block.block_id,
            block.parent_id,
            state.lib(),
            block.our_txs,
            block.channel_txs,
            Vec::new(),
        );
    }

    result
}

/// Fetch a finalized slot range, then apply it without another suspension
/// point. Dropping the caller's future during any node request leaves
/// `state` untouched, so retry starts from the same boundary.
pub(super) async fn fetch_and_process_blocks<Node>(
    state: &mut TxState,
    from_slot: u64,
    to_slot: u64,
    channel_id: ChannelId,
    node: &Node,
) -> Result<FetchedBatch, Error>
where
    Node: adapter::Node + Sync,
{
    let prepared =
        prepare_finalized_blocks(from_slot, to_slot, channel_id, node, Some(state)).await?;

    Ok(apply_finalized_blocks(state, prepared))
}

/// Fetch the deposit-amount lookup for a single block, gated on whether the
/// block has any deposit op for our channel.
///
/// Per node semantics, a block and its events are atomically visible — so a
/// block containing a deposit op must yield an event for that op. The
/// returned `HashMap` is therefore the *complete* `(tx_hash, op_id) → amount`
/// lookup for every deposit op of our channel in this block.
///
/// On any failure (HTTP error, `Ok(None)`, or events missing an entry for
/// some deposit op) we log at error level and return [`Error::Network`]. The
/// caller's contract is "either retry, or abandon this block" — never
/// silently emit a partial result, because that drops real deposits.
async fn fetch_block_deposit_events<Node>(
    node: &Node,
    block_id: HeaderId,
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
) -> Result<DepositEvents, Error>
where
    Node: adapter::Node + Sync,
{
    let expected: Vec<DepositOpKey> = transactions
        .iter()
        .flat_map(|tx| {
            let tx_hash = tx.op_refs().hash();
            tx.op_refs().into_iter().filter_map(move |op| match op {
                OpRef::ChannelDeposit(d) if d.channel_id == channel_id => Some(DepositOpKey {
                    tx_hash,
                    op_id: d.op_id(),
                }),
                _ => None,
            })
        })
        .collect();

    if expected.is_empty() {
        return Ok(DepositEvents::new());
    }

    let events = match node.block_events(block_id).await {
        Ok(Some(events)) => events,
        Ok(None) => {
            error!(
                target: TARGET,
                ?block_id,
                "Events endpoint returned no body for a block with a channel deposit; \
                 events should be atomically visible with the block"
            );
            return Err(Error::Network(format!(
                "no events for block {block_id} containing channel deposits"
            )));
        }
        Err(err) => {
            error!(target: TARGET, ?block_id, ?err, "Failed to fetch events for block");
            return Err(Error::Network(format!(
                "failed to fetch events for block {block_id}: {err}"
            )));
        }
    };

    let deposit_events = build_deposit_events(&events);
    for key in &expected {
        if !deposit_events.contains_key(key) {
            error!(
                target: TARGET,
                ?block_id,
                tx_hash = ?key.tx_hash,
                op_id = ?key.op_id,
                "Block events missing an entry for a known channel deposit op; \
                 expected atomic block/events visibility per node semantics"
            );
            return Err(Error::Network(format!(
                "block {block_id} events missing deposit entry for tx {:?} op {:?}",
                key.tx_hash, key.op_id
            )));
        }
    }
    Ok(deposit_events)
}

/// Walks `transactions` and groups channel-relevant ops per Mantle tx,
/// preserving on-chain execution order both across and within txs.
///
/// Each returned [`FinalizedTx`] corresponds to one Mantle tx that touched
/// our channel. Its `ops` are in op order: a tx with `Deposit + Inscribe`
/// emits `[Deposit, Inscribe]`. Atomicity is structural — every op inside
/// the same [`FinalizedTx`] succeeded together on chain.
///
/// The channel protocol guarantees a linear parent-child chain per channel
/// within a block, so tx order already equals parent-chain order. We do NOT
/// reorder — the trust assumption (each `ChannelInscribe`'s `parent` chains
/// off the running tip) is asserted by [`classify_channel_txs`], which
/// every caller runs on the same `transactions` before this walker.
///
/// Deposits without a matching event entry are skipped with a warning.
fn extract_finalized_items(
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
    l1_slot: Slot,
    deposit_events: &DepositEvents,
) -> Vec<FinalizedTx> {
    let mut items: Vec<FinalizedTx> = Vec::new();

    for tx in transactions {
        let tx_hash = tx.op_refs().hash();
        let mut ops: Vec<FinalizedOp> = Vec::new();
        for op in tx.op_refs() {
            match op {
                OpRef::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                    // Chain order is asserted by `classify_channel_txs`,
                    // which runs on the same `transactions` before this
                    // walker on every call site (live + backfill).
                    let info = InscriptionInfo {
                        tx_hash,
                        parent_msg: inscribe.parent,
                        this_msg: inscribe.id(),
                        payload: inscribe.inscription.clone(),
                        signer: Some(inscribe.signer),
                    };
                    ops.push(FinalizedOp::Inscription(info));
                }
                OpRef::ChannelConfig(config) if config.channel == channel_id => {
                    ops.push(FinalizedOp::Config(InscriptionInfo {
                        tx_hash,
                        parent_msg: config.parent,
                        this_msg: config.id(),
                        payload: Inscription::new_unchecked(Vec::new()),
                        signer: None,
                    }));
                }
                OpRef::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
                    let op_id = deposit.op_id();
                    // `fetch_block_deposit_events` validates that every
                    // channel-deposit op in the block has a matching event
                    // entry before returning, so the lookup is infallible
                    // here. A miss would be a caller-side bug.
                    let event = deposit_events.get(&DepositOpKey { tx_hash, op_id }).expect(
                        "deposit_events must contain every channel deposit op - \
                         fetch_block_deposit_events invariant",
                    );
                    ops.push(FinalizedOp::Deposit(DepositInfo {
                        tx_hash,
                        op_id,
                        channel_id,
                        inputs: deposit.inputs.clone(),
                        notes: event.notes.clone(),
                        amount: event.amount,
                        metadata: deposit.metadata.clone(),
                    }));
                }
                OpRef::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
                    ops.push(FinalizedOp::Withdraw(WithdrawInfo {
                        tx_hash,
                        op: (*withdraw).clone(),
                    }));
                }
                OpRef::ChannelTransfer(transfer) if transfer.channel_id == channel_id => {
                    ops.push(FinalizedOp::ChannelTransfer(ChannelTransferInfo {
                        tx_hash,
                        op: (*transfer).clone(),
                    }));
                }
                _ => {}
            }
        }
        if !ops.is_empty() {
            items.push(FinalizedTx {
                tx_hash,
                l1_slot,
                ops,
            });
        }
    }

    items
}

/// Walk backwards from `from` until reaching a block already present in the
/// current state or the finalized batch prepared for this event. Returns
/// blocks in forward order (oldest first) without mutating state.
fn block_is_known(
    state: Option<&TxState>,
    additionally_known: &HashSet<HeaderId>,
    lib: HeaderId,
    block: HeaderId,
) -> bool {
    block == lib
        || additionally_known.contains(&block)
        || state.is_some_and(|state| state.has_block(&block))
}

/// The unknown ancestors of `from` back to a known block, oldest first. A
/// block that cannot be fetched fails the event: applying the live block over
/// a hole would cut every branch walk short of LIB, and the hole would never
/// be revisited since the next event's parent is then known.
async fn walk_back_to_known<Node>(
    state: Option<&TxState>,
    additionally_known: &HashSet<HeaderId>,
    lib: HeaderId,
    from: HeaderId,
    node: &Node,
) -> Result<Vec<ApiBlock>, Error>
where
    Node: adapter::Node + Sync,
{
    debug!(target: TARGET, "Backfilling canonical chain from {from:?}");

    let mut blocks = Vec::new();
    let mut current = from;

    while !block_is_known(state, additionally_known, lib, current) {
        let block = fetch_backfill_block(node, current).await?;
        current = block.header.parent_block;
        blocks.push(block);
    }

    blocks.reverse();
    debug!(target: TARGET, blocks = blocks.len(), "Canonical backfill prepared");
    Ok(blocks)
}

/// Prepare each canonical-backfill block with its channel-note ops, and
/// collect the deposits those blocks carry, in block order. Each block needs
/// a deposit-events fetch, so this runs in the prepare phase, keeping apply
/// await-free; a failed fetch fails the event, like the live block's.
async fn prepare_backfill_blocks<Node>(
    blocks: Vec<ApiBlock>,
    channel_id: ChannelId,
    node: &Node,
) -> Result<(Vec<(ApiBlock, Vec<NoteOp>)>, Vec<DepositInfo>), Error>
where
    Node: adapter::Node + Sync,
{
    let mut prepared = Vec::with_capacity(blocks.len());
    let mut deposits = Vec::new();
    for block in blocks {
        let deposit_events =
            fetch_block_deposit_events(node, block.header.id, &block.transactions, channel_id)
                .await?;
        let note_ops = note_ops_from_txs(
            &block.transactions,
            channel_id,
            &deposit_events,
            block.header.slot,
        );
        deposits.extend(block_channel_deposits(
            &block.transactions,
            channel_id,
            block.header.slot,
            &deposit_events,
        ));
        prepared.push((block, note_ops));
    }
    Ok((prepared, deposits))
}

async fn fetch_backfill_block<Node>(node: &Node, block_id: HeaderId) -> Result<ApiBlock, Error>
where
    Node: adapter::Node + Sync,
{
    match node.block(block_id).await {
        Ok(Some(block)) => Ok(block),
        Ok(None) => {
            error!(target: TARGET, ?block_id, "Block not found during canonical backfill");
            Err(Error::Network(format!(
                "block {block_id} not found during canonical backfill"
            )))
        }
        Err(error) => {
            error!(target: TARGET, ?block_id, %error, "Failed to fetch block during canonical backfill");
            Err(Error::Network(format!(
                "failed to fetch block {block_id} during canonical backfill: {error}"
            )))
        }
    }
}

fn apply_backfilled_block(
    state: &mut TxState,
    block: &ApiBlock,
    channel_id: ChannelId,
    lib: HeaderId,
    note_ops: Vec<NoteOp>,
) {
    let block_id = block.header.id;
    let parent_id = block.header.parent_block;

    let our_txs: Vec<TxHash> = block
        .transactions
        .iter()
        .filter(|tx| touches_channel_tip(tx, channel_id))
        .map(|tx| tx.op_refs().hash())
        .collect();

    let mut channel_txs = classify_channel_txs(&block.transactions, channel_id);
    demote_non_identity_pin_deposits(&mut channel_txs, &block.transactions, channel_id, state);

    let mirrorable = mirrorable_txs(&channel_txs, &block.transactions);
    // Use current state lib to avoid premature finalization
    state.process_block(block_id, parent_id, lib, our_txs, channel_txs, note_ops);
    state.store_block_signed_txs(block_id, mirrorable);
}

/// The block's txs the mirror re-posts: every classified channel tx.
fn mirrorable_txs(
    classified: &[BlockChannelTx],
    transactions: &[SignedOps<Unverified, StandardMode>],
) -> Vec<SignedOps<Unverified, StandardMode>> {
    let mirrorable: HashSet<TxHash> = classified
        .iter()
        .filter_map(BlockChannelTx::tx_hash)
        .collect();
    transactions
        .iter()
        .filter(|tx| mirrorable.contains(&tx.hash()))
        .cloned()
        .collect()
}

/// Classify a block's channel-touching txs in tx-then-op order: a `publish`
/// inscription, an atomic bundle, or a custom shape the SDK cannot produce.
/// `ChannelConfig` ops chain on the channel's own config lineage, never
/// touch the message tip, and yield no entries.
///
/// The ledger validates ops in tx-then-op order, with each `ChannelInscribe`
/// requiring `parent == channel.tip_message`. A block in which tip-advancing
/// ops for one channel appear out of chain order would fail validation, so
/// tx-then-op order is already chain order — callers (e.g. `channel_tip_at`)
/// can rely on `last()` being the post-block tip. We verify this trust
/// assumption with an inline assertion: each `ChannelInscribe`'s `parent`
/// must equal the running in-block tip. A mismatch panics rather than
/// silently re-deriving order, because the same node bug could produce an
/// undetectable mis-ordering elsewhere.
fn classify_channel_txs(
    txs: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
) -> Vec<BlockChannelTx> {
    // Running in-block channel tip, for the chain-order assertion.
    let mut block_tip: Option<MsgId> = None;
    txs.iter()
        .filter_map(|tx| classify_channel_tx(tx, channel_id, &mut block_tip))
        .collect()
}

/// Classify one tx's channel ops; `None` when the tx has no tip-advancing op.
pub(super) fn classify_channel_tx(
    tx: &SignedOps<Unverified, StandardMode>,
    channel_id: ChannelId,
    block_tip: &mut Option<MsgId>,
) -> Option<BlockChannelTx> {
    let tx_hash = tx.op_refs().hash();
    let mut entries: Vec<InscriptionInfo> = Vec::new();
    let mut config_entries: Vec<InscriptionInfo> = Vec::new();
    let mut inscribes = 0usize;
    let mut configs = 0usize;
    let mut withdraws: Vec<WithdrawInfo> = Vec::new();
    let mut transfers = 0usize;
    let mut channel_transfers = 0usize;
    let mut channel_transfer_inputs: Option<Inputs> = None;
    let mut foreign_ops = false;

    for op in tx.op_refs() {
        match op {
            OpRef::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                if let Some(prev) = *block_tip {
                    assert_eq!(
                        inscribe.parent, prev,
                        "block delivered inscription out of execution order: \
                         inscribe.parent {:?} does not chain off the prior in-block tip {:?}",
                        inscribe.parent, prev
                    );
                }
                inscribes += 1;
                let this_msg = inscribe.id();
                entries.push(InscriptionInfo {
                    tx_hash,
                    parent_msg: inscribe.parent,
                    this_msg,
                    payload: inscribe.inscription.clone(),
                    signer: Some(inscribe.signer),
                });
                *block_tip = Some(this_msg);
            }
            OpRef::ChannelConfig(config) if config.channel == channel_id => {
                configs += 1;
                // Configs sit on the separate config lineage — `this_msg` is a
                // config id, `parent_msg` its config parent, payload empty.
                // Captured here (including inside mixed/custom txs) so the
                // config-tip walk can see every landed config, not just the
                // node's single tip.
                config_entries.push(InscriptionInfo {
                    tx_hash,
                    parent_msg: config.parent,
                    this_msg: config.id(),
                    payload: [].into(),
                    signer: None,
                });
            }
            OpRef::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
                withdraws.push(WithdrawInfo {
                    tx_hash,
                    op: (*withdraw).clone(),
                });
            }
            OpRef::ChannelTransfer(transfer) if transfer.channel_id == channel_id => {
                channel_transfers += 1;
                channel_transfer_inputs = Some(transfer.inputs.clone());
            }
            OpRef::Transfer(_) => transfers += 1,
            _ => foreign_ops = true,
        }
    }

    if entries.is_empty() && config_entries.is_empty() {
        // Neither a tip-advancing op nor a config — nothing to store.
        return None;
    }

    let clean = !foreign_ops && transfers <= 1;
    Some(
        if clean && inscribes == 1 && configs == 0 && channel_transfers <= 1 {
            let inscription = entries.pop().expect("exactly one inscribe entry");
            match (withdraws.is_empty(), channel_transfers) {
                // `[inscribe, channel_transfer, withdraw…]`. Only re-issue of our
                // own orphaned bundle needs the outputs; an observed one has none.
                (false, _) => BlockChannelTx::AtomicWithdraw(AtomicWithdrawInfo {
                    tx_hash,
                    inscription,
                    withdraws,
                    outputs: Outputs::empty(),
                }),
                // `[inscribe, channel_transfer]` — transfer consumes the deposited note.
                (true, 1) => BlockChannelTx::PinDeposit(PinDepositInfo {
                    tx_hash,
                    inscription,
                    consumed_notes: channel_transfer_inputs
                        .expect("channel_transfers == 1 implies a captured transfer"),
                }),
                (true, _) => BlockChannelTx::Inscription(inscription),
            }
        } else if clean
            && configs == 1
            && inscribes == 0
            && withdraws.is_empty()
            && channel_transfers == 0
        {
            // A pure single-config tx — the config-lineage analogue of a clean
            // single inscription.
            BlockChannelTx::Config(config_entries.pop().expect("exactly one config entry"))
        } else {
            BlockChannelTx::Custom {
                tx: tx.clone(),
                message_entries: entries,
                config_entries,
            }
        },
    )
}

/// Whether `tx` is a clean single-config tx for `channel_id` — the same
/// config-only shape [`classify_channel_tx`] reports as
/// [`BlockChannelTx::Config`]. Mirrors that rule so a shed config is typed the
/// same way it was classified on chain.
pub(super) fn is_pure_config<Mode: VerificationMode>(
    tx: &SignedOps<Unverified, Mode>,
    channel_id: ChannelId,
) -> bool {
    let mut configs = 0usize;
    let mut transfers = 0usize;
    for op in tx.op_refs_iter() {
        match op {
            OpRef::ChannelConfig(config) if config.channel == channel_id => configs += 1,
            OpRef::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => return false,
            OpRef::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => return false,
            OpRef::Transfer(_) => transfers += 1,
            _ => return false,
        }
    }
    configs == 1 && transfers <= 1
}

/// Type a shed pending tx for orphan reporting: a config-only tx as
/// [`ChannelUpdateTx::Config`], anything else as [`ChannelUpdateTx::Custom`].
pub(super) fn classify_shed_other(
    tx: SignedOps<Unverified, StandardMode>,
    channel_id: ChannelId,
) -> ChannelUpdateTx {
    if is_pure_config(&tx, channel_id) {
        ChannelUpdateTx::Config(tx)
    } else {
        ChannelUpdateTx::Custom(tx)
    }
}

/// True iff this tx contains any op that advances our channel's tip pointer
/// (`ChannelInscribe` or `ChannelConfig`). Deposits and withdraws don't move
/// the tip and so don't make a tx "ours" for tip-tracking purposes.
fn touches_channel_tip<State: VerificationState, Mode: VerificationMode>(
    tx: &SignedOps<State, Mode>,
    channel_id: ChannelId,
) -> bool {
    tx.op_refs().iter().any(|op| match op {
        OpRef::ChannelInscribe(inscribe) => inscribe.channel_id == channel_id,
        OpRef::ChannelConfig(set_keys) => set_keys.channel == channel_id,
        _ => false,
    })
}

#[cfg(test)]
mod tests;
