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
use tracing::{debug, error, warn};

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
    /// Inscriptions that appeared in this block. Surfaced so a consumer learns
    /// its tx reached the chain (`OnChain` status) even when the tx didn't move
    /// the canonical channel chain.
    pub(super) mined_inscriptions: Vec<InscriptionInfo>,
    /// Channel deposits observed in this block, in op order. Surfaced
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
    mined_inscriptions: Vec<InscriptionInfo>,
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
    let canonical_backfill = if parent_known {
        Vec::new()
    } else {
        let blocks =
            walk_back_to_known(state, &finalized_block_ids, state_lib, parent_id, node).await;
        prepare_backfill_note_ops(blocks, channel_id, node).await
    };

    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| touches_channel_tip(tx, channel_id))
        .map(|tx| tx.op_refs().hash())
        .collect();
    let channel_txs = classify_channel_txs(&event.block.transactions, channel_id);
    let mut mined_inscriptions: Vec<InscriptionInfo> = channel_txs
        .iter()
        .flat_map(BlockChannelTx::infos)
        .cloned()
        .collect();
    let config_entries =
        mined_config_entries(&event.block.transactions, channel_id, &mined_inscriptions);
    mined_inscriptions.extend(config_entries);

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
    let deposits = block_channel_deposits(
        &event.block.transactions,
        channel_id,
        event.block.header.slot,
        &deposit_events,
    );

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
        mined_inscriptions,
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
        mined_inscriptions,
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

    // Remove our pending txs that were finalized in the backfilled LIB blocks.
    // `finalized_items` already carries the typed payloads (built before
    // pending was mutated) so we just need to clean up state here.
    for tx_hash in &finalized_batch.our_tx_hashes {
        s.remove_pending(tx_hash);
    }

    *current_tip = Some(tip);

    mirror_branch_from_store(s, tip, channel_id);

    // Detect channel changes by diffing the channel view on every block. On
    // the first event there is no old tip: what restored pending chains on
    // was the view before the restart, and the rest of the channel is new
    // (a clean start on an existing channel).
    let finalized_now: HashSet<MsgId> = finalized_batch
        .items
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .filter_map(|op| match op {
            FinalizedOp::Inscription(i) | FinalizedOp::Config(i) => Some(i.this_msg),
            _ => None,
        })
        .collect();

    let old_lineage = old_lineage.unwrap_or_else(|| s.lineage_under(tip, &tracked_before));
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
    let old_txs: HashSet<TxHash> = old_lineage.iter().map(|info| info.tx_hash).collect();
    let common_prefix = s
        .channel_view_txs(tip, &finalized_now)
        .into_iter()
        .filter(|tx| old_txs.contains(&tx.tx_hash()))
        .collect();

    BlockEventResult {
        finalized_items: finalized_batch.items,
        channel_update,
        common_prefix,
        mined_inscriptions,
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

/// Configs are not in `mined`, but their txs still need `OnChain`
/// status events: one entry per config-carrying tx, with its config-lineage
/// ids. Status is keyed on the tx, so a tx already covered by an inscription
/// entry in `mined` needs nothing more.
fn mined_config_entries(
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
    mined: &[InscriptionInfo],
) -> Vec<InscriptionInfo> {
    transactions
        .iter()
        .filter_map(|tx| {
            let tx_hash = tx.hash();
            if mined.iter().any(|info| info.tx_hash == tx_hash) {
                return None;
            }
            channel_configs(tx, channel_id).into_iter().next()
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

async fn walk_back_to_known<Node>(
    state: Option<&TxState>,
    additionally_known: &HashSet<HeaderId>,
    lib: HeaderId,
    from: HeaderId,
    node: &Node,
) -> Vec<ApiBlock>
where
    Node: adapter::Node + Sync,
{
    debug!(target: TARGET, "Backfilling canonical chain from {from:?}");

    let mut blocks = Vec::new();
    let mut current = from;

    while !block_is_known(state, additionally_known, lib, current) {
        let Some(block) = fetch_backfill_block(node, current).await else {
            break;
        };

        current = block.header.parent_block;
        blocks.push(block);
    }

    blocks.reverse();
    debug!(target: TARGET, blocks = blocks.len(), "Canonical backfill prepared");
    blocks
}

/// [`fetch_block_deposit_events`] with the canonical-backfill error contract:
/// `None` (after a warn) tells the caller to stop applying blocks.
async fn backfill_deposit_events<Node>(
    node: &Node,
    block: &ApiBlock,
    channel_id: ChannelId,
) -> Option<DepositEvents>
where
    Node: adapter::Node + Sync,
{
    match fetch_block_deposit_events(node, block.header.id, &block.transactions, channel_id).await {
        Ok(events) => Some(events),
        Err(e) => {
            warn!(
                target: TARGET,
                "Failed to fetch deposit events during canonical backfill: {e}"
            );
            None
        }
    }
}

/// Prepare each canonical-backfill block with its channel-note ops. Each needs
/// a deposit-events fetch, so this runs in the prepare phase, keeping apply
/// await-free. Best-effort: on a fetch failure, stop and keep the prefix
/// already prepared — the rest is retried on the next event.
async fn prepare_backfill_note_ops<Node>(
    blocks: Vec<ApiBlock>,
    channel_id: ChannelId,
    node: &Node,
) -> Vec<(ApiBlock, Vec<NoteOp>)>
where
    Node: adapter::Node + Sync,
{
    let mut prepared = Vec::with_capacity(blocks.len());
    for block in blocks {
        let Some(deposit_events) = backfill_deposit_events(node, &block, channel_id).await else {
            break;
        };
        let note_ops = note_ops_from_txs(
            &block.transactions,
            channel_id,
            &deposit_events,
            block.header.slot,
        );
        prepared.push((block, note_ops));
    }
    prepared
}

async fn fetch_backfill_block<Node>(node: &Node, block_id: HeaderId) -> Option<ApiBlock>
where
    Node: adapter::Node + Sync,
{
    match node.block(block_id).await {
        Ok(Some(block)) => Some(block),
        Ok(None) => {
            warn!(target: TARGET, ?block_id, "Block not found during canonical backfill");
            None
        }
        Err(error) => {
            warn!(target: TARGET, ?block_id, %error, "Failed to fetch block during canonical backfill");
            None
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

    // Mirror inscriptions into pending before the safe-set build, matching
    // the live-block path in `handle_block_event`.
    mirror_channel_txs(state, &channel_txs, &block.transactions, channel_id);

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
mod tests {
    use lb_core::{
        crypto::Hash,
        events::{DepositNote, DepositRecreatedNotes},
        mantle::{
            Note, NoteId, Op, Value,
            channel::{SlotTimeframe, SlotTimeout},
            ops::{
                OpProof,
                channel::{
                    VerifiedChannelKeys,
                    config::ChannelConfigOp,
                    deposit::{DepositOp, Metadata},
                    inscribe::InscriptionOp,
                    withdraw::ChannelWithdrawOp,
                },
            },
            transactions::{OpProofs, Ops},
        },
    };
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};

    use super::*;
    use crate::{
        adapter::DepositEvent,
        sequencer::types::TxSource,
        test_support::{
            MockNode, api_block, deposit_event, header_id, inscribe_op, live_event,
            unverified_tx_with_ops,
        },
    };

    fn deposit_op(channel_id: ChannelId, input_seed: u32, metadata: Metadata) -> DepositOp {
        DepositOp {
            channel_id,
            inputs: Inputs::new([NoteId::from(Fr::from(input_seed))]),
            metadata,
        }
    }

    fn deposit_event_entry(
        tx_hash: TxHash,
        op_id: Hash,
        amount: Value,
    ) -> (DepositOpKey, DepositEvent) {
        (
            DepositOpKey { tx_hash, op_id },
            DepositEvent {
                amount,
                notes: DepositRecreatedNotes::default(),
            },
        )
    }

    /// Extract deposits via the unified walker and filter to deposit entries
    /// for assertion clarity.
    fn extract_deposits_for_test(
        transactions: &[SignedOps<Unverified, StandardMode>],
        channel_id: ChannelId,
        deposit_events: &DepositEvents,
    ) -> Vec<DepositInfo> {
        extract_finalized_items(transactions, channel_id, Slot::from(0), deposit_events)
            .into_iter()
            .flat_map(|t| t.ops.into_iter())
            .filter_map(|op| match op {
                FinalizedOp::Deposit(d) => Some(d),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn extract_deposits_returns_matching_amount() {
        let channel_id = ChannelId::from([0; 32]);
        let other_channel = ChannelId::from([1; 32]);

        let deposit_for_us = deposit_op(channel_id, 1, b"to Alice".into());
        let deposit_other_channel = deposit_op(other_channel, 2, b"to Bob".into());
        let our_op_id = deposit_for_us.op_id();

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelDeposit(deposit_for_us.clone()),
            Op::ChannelDeposit(deposit_other_channel),
        ]);
        let tx_hash = tx.hash();

        let amounts = DepositEvents::from([deposit_event_entry(tx_hash, our_op_id, 1234)]);

        let deposits = extract_deposits_for_test(std::slice::from_ref(&tx), channel_id, &amounts);
        assert_eq!(
            deposits.len(),
            1,
            "only deposit on our channel is extracted"
        );
        let d = &deposits[0];
        assert_eq!(d.channel_id, channel_id);
        assert_eq!(d.tx_hash, tx_hash);
        assert_eq!(d.op_id, our_op_id);
        assert_eq!(d.amount, 1234);
        assert_eq!(d.metadata, b"to Alice".into());
        assert_eq!(d.inputs, deposit_for_us.inputs);
    }

    #[test]
    #[should_panic(expected = "fetch_block_deposit_events invariant")]
    fn extract_finalized_items_panics_if_deposit_events_incomplete() {
        // The walker contract: `deposit_events` must contain an entry for
        // every channel-deposit op in the input transactions. This is
        // enforced upstream by `fetch_block_deposit_events`, which validates
        // completeness and errors out before the walker is ever called with a
        // gap. A panic here surfaces the bug immediately if a future caller
        // violates that invariant — silent skip would drop a real deposit.
        let channel_id = ChannelId::from([0; 32]);
        let op = deposit_op(channel_id, 1, b"to Alice".into());
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(op)]);
        drop(extract_finalized_items(
            std::slice::from_ref(&tx),
            channel_id,
            Slot::from(0),
            &HashMap::new(),
        ));
    }

    #[test]
    fn extract_deposits_preserves_tx_and_op_order() {
        let channel_id = ChannelId::from([0; 32]);
        let d1 = deposit_op(channel_id, 1, b"first".into());
        let d2 = deposit_op(channel_id, 2, b"second".into());
        let d3 = deposit_op(channel_id, 3, b"third".into());
        let id1 = d1.op_id();
        let id2 = d2.op_id();
        let id3 = d3.op_id();

        // tx_a carries d1 then d2 (in op order); tx_b carries d3.
        let tx_a = unverified_tx_with_ops(vec![Op::ChannelDeposit(d1), Op::ChannelDeposit(d2)]);
        let tx_b = unverified_tx_with_ops(vec![Op::ChannelDeposit(d3)]);
        let hash_a = tx_a.hash();
        let hash_b = tx_b.hash();

        let amounts = DepositEvents::from([
            deposit_event_entry(hash_a, id1, 10),
            deposit_event_entry(hash_a, id2, 20),
            deposit_event_entry(hash_b, id3, 30),
        ]);

        let deposits = extract_deposits_for_test(&[tx_a, tx_b], channel_id, &amounts);
        let metadata_in_order: Vec<&[u8]> =
            deposits.iter().map(|d| d.metadata.as_slice()).collect();
        assert_eq!(
            metadata_in_order,
            vec![b"first" as &[u8], b"second", b"third"],
            "deposits emitted in tx/op order across transactions"
        );
    }

    #[test]
    fn extract_finalized_items_interleaves_deposit_then_inscription_in_same_tx() {
        // The atomic deposit+inscription pattern: one Mantle tx with
        // [ChannelDeposit, ChannelInscribe]. The bridge use case requires the
        // deposit to be emitted BEFORE the inscription so that consumers
        // (e.g. LEZ) can validate references from the inscription back to
        // the just-finalized deposit.
        let channel_id = ChannelId::from([0; 32]);
        let dep = deposit_op(channel_id, 1, b"deposit-meta".into());
        let dep_op_id = dep.op_id();
        let inscribe = InscriptionOp {
            channel_id,
            parent: MsgId::root(),
            inscription: Inscription::new_unchecked(Vec::new()),
            signer: Ed25519Key::from_bytes(&[0; 32])
                .public_key()
                .into_unverified(),
        };

        let tx =
            unverified_tx_with_ops(vec![Op::ChannelDeposit(dep), Op::ChannelInscribe(inscribe)]);
        let tx_hash = tx.hash();

        let mut amounts = DepositEvents::new();
        amounts.insert(
            DepositOpKey {
                tx_hash,
                op_id: dep_op_id,
            },
            DepositEvent {
                amount: 500,
                notes: DepositRecreatedNotes::default(),
            },
        );

        let items = extract_finalized_items(
            std::slice::from_ref(&tx),
            channel_id,
            Slot::from(42),
            &amounts,
        );

        assert_eq!(items.len(), 1, "one FinalizedTx for the single Mantle tx");
        assert_eq!(items[0].tx_hash, tx_hash);
        assert_eq!(items[0].l1_slot, Slot::from(42));
        assert_eq!(items[0].ops.len(), 2);
        assert!(matches!(items[0].ops[0], FinalizedOp::Deposit(_)));
        assert!(matches!(items[0].ops[1], FinalizedOp::Inscription(_)));
    }

    #[test]
    fn deposit_plus_inscription_is_adopted_as_custom() {
        // The atomic bridge pattern: [ChannelDeposit, ChannelInscribe] in
        // one tx. The SDK cannot rebuild a deposit, so the tx classifies as
        // `Custom`: its payload still reaches the consumer via `adopted`
        // (typed so it isn't republished as a bare message, which could
        // race and invalidate the author's deposit), and it is not mirrored
        // for retry — its author recovers it.
        let channel_id = ChannelId::from([0; 32]);
        let dep = deposit_op(channel_id, 1, b"deposit-meta".into());
        let inscribe = inscribe_op(channel_id, MsgId::root(), b"bridge");
        let msg_id = inscribe.id();
        let tx =
            unverified_tx_with_ops(vec![Op::ChannelDeposit(dep), Op::ChannelInscribe(inscribe)]);
        let tx_hash = tx.hash();

        let classified = classify_channel_txs(std::slice::from_ref(&tx), channel_id);
        assert_eq!(classified.len(), 1);
        assert!(
            matches!(&classified[0], BlockChannelTx::Custom { message_entries, .. } if message_entries.len() == 1)
        );

        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        let old_lineage = state.channel_lineage(genesis);
        mirror_channel_txs(
            &mut state,
            &classified,
            std::slice::from_ref(&tx),
            channel_id,
        );
        state.process_block(
            block,
            genesis,
            genesis,
            vec![tx_hash],
            classified,
            Vec::new(),
        );

        assert!(
            state.is_tracked(&tx_hash),
            "custom tx is mirrored for retry"
        );
        assert_eq!(state.tx_source(&tx_hash), TxSource::Other);
        let update = state
            .detect_channel_update(&old_lineage, block, &HashSet::new())
            .expect("update");
        assert_eq!(update.new_channel_tip, msg_id);
        assert_eq!(update.adopted.len(), 1);
        match &update.adopted[0] {
            ChannelUpdateTx::Custom(adopted_tx) => {
                assert_eq!(
                    adopted_tx.op_refs().hash(),
                    tx_hash,
                    "the whole tx is handed over"
                );
                let inscriptions = channel_inscriptions(adopted_tx, channel_id);
                assert_eq!(inscriptions.len(), 1);
                assert_eq!(inscriptions[0].this_msg, msg_id);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    /// An `[inscribe, channel_transfer]` whose transfer re-creates its inputs
    /// unchanged is a sound `PinDeposit`.
    #[test]
    fn inscribe_plus_identity_transfer_is_pin_deposit() {
        use crate::sequencer::types::ChannelNote;

        let channel_id = ChannelId::from([0; 32]);
        let pk = lb_key_management_system_service::keys::ZkPublicKey::from(Fr::from(7u64));
        let input_id = NoteId::from(Fr::from(42u64));

        let inscribe = inscribe_op(channel_id, MsgId::root(), b"pin");
        let msg_id = inscribe.id();
        let transfer = ChannelTransferOp {
            channel_id,
            inputs: Inputs::new([input_id]),
            outputs: Outputs::new([Note::new(50, pk)]),
        };
        let tx = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(inscribe),
            Op::ChannelTransfer(transfer),
        ]);
        let tx_hash = tx.hash();

        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        state.apply_finalized_note_ops(vec![NoteOp::Add(ChannelNote {
            note_id: input_id,
            value: 50,
            pk,
            slot: Slot::from(0),
        })]);
        let old_lineage = state.channel_lineage(genesis);

        let mut classified = classify_channel_txs(std::slice::from_ref(&tx), channel_id);
        demote_non_identity_pin_deposits(
            &mut classified,
            std::slice::from_ref(&tx),
            channel_id,
            &state,
        );
        assert!(matches!(&classified[0], BlockChannelTx::PinDeposit(_)));

        mirror_channel_txs(
            &mut state,
            &classified,
            std::slice::from_ref(&tx),
            channel_id,
        );
        state.process_block(
            block,
            genesis,
            genesis,
            vec![tx_hash],
            classified,
            Vec::new(),
        );

        let update = state
            .detect_channel_update(&old_lineage, block, &HashSet::new())
            .expect("update");
        assert_eq!(update.new_channel_tip, msg_id);
        assert_eq!(update.adopted.len(), 1);
        match &update.adopted[0] {
            ChannelUpdateTx::PinDeposit(a) => {
                assert_eq!(a.consumed_notes, Inputs::new([input_id]));
            }
            other => panic!("expected PinDeposit, got {other:?}"),
        }
    }

    /// A re-keying transfer (balance-preserving, but not an identity
    /// re-creation) falls to `Custom` — its payload still reaches the consumer
    /// via `adopted`, and it is not mirrored for retry.
    #[test]
    fn inscribe_plus_non_identity_transfer_is_custom() {
        use crate::sequencer::types::ChannelNote;

        let channel_id = ChannelId::from([0; 32]);
        let pk = lb_key_management_system_service::keys::ZkPublicKey::from(Fr::from(7u64));
        let rekeyed = lb_key_management_system_service::keys::ZkPublicKey::from(Fr::from(8u64));
        let input_id = NoteId::from(Fr::from(42u64));

        let inscribe = inscribe_op(channel_id, MsgId::root(), b"pin");
        let msg_id = inscribe.id();
        let transfer = ChannelTransferOp {
            channel_id,
            inputs: Inputs::new([input_id]),
            outputs: Outputs::new([Note::new(50, rekeyed)]),
        };
        let tx = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(inscribe),
            Op::ChannelTransfer(transfer),
        ]);
        let tx_hash = tx.hash();

        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        state.apply_finalized_note_ops(vec![NoteOp::Add(ChannelNote {
            note_id: input_id,
            value: 50,
            pk,
            slot: Slot::from(0),
        })]);
        let old_lineage = state.channel_lineage(genesis);

        let mut classified = classify_channel_txs(std::slice::from_ref(&tx), channel_id);
        demote_non_identity_pin_deposits(
            &mut classified,
            std::slice::from_ref(&tx),
            channel_id,
            &state,
        );
        assert!(
            matches!(&classified[0], BlockChannelTx::Custom { message_entries, .. } if message_entries.len() == 1)
        );

        mirror_channel_txs(
            &mut state,
            &classified,
            std::slice::from_ref(&tx),
            channel_id,
        );
        state.process_block(
            block,
            genesis,
            genesis,
            vec![tx_hash],
            classified,
            Vec::new(),
        );

        assert!(
            state.is_tracked(&tx_hash),
            "non-identity bundle is mirrored for retry"
        );
        assert_eq!(state.tx_source(&tx_hash), TxSource::Other);
        let update = state
            .detect_channel_update(&old_lineage, block, &HashSet::new())
            .expect("update");
        assert_eq!(update.new_channel_tip, msg_id);
        assert_eq!(update.adopted.len(), 1);
        match &update.adopted[0] {
            ChannelUpdateTx::Custom(adopted_tx) => {
                assert_eq!(adopted_tx.hash(), tx_hash);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn classify_shed_other_types_config_only_as_config_and_the_rest_as_custom() {
        let channel_id = ChannelId::from([0; 32]);

        // A config-only tx is typed Config.
        let config_tx = unverified_tx_with_ops(vec![Op::ChannelConfig(channel_config(
            channel_id,
            MsgId::root(),
        ))]);
        let config_hash = config_tx.hash();
        match classify_shed_other(config_tx, channel_id) {
            ChannelUpdateTx::Config(tx) => assert_eq!(tx.hash(), config_hash),
            other => panic!("expected Config, got {other:?}"),
        }

        // A config bundled with an inscription is not config-only → Custom.
        let mixed = unverified_tx_with_ops(vec![
            Op::ChannelConfig(channel_config(channel_id, MsgId::root())),
            Op::ChannelInscribe(inscribe_op(channel_id, MsgId::root(), b"m")),
        ]);
        assert!(matches!(
            classify_shed_other(mixed, channel_id),
            ChannelUpdateTx::Custom(_)
        ));

        // A tx with no config → Custom.
        let no_config = unverified_tx_with_ops(vec![Op::ChannelInscribe(inscribe_op(
            channel_id,
            MsgId::root(),
            b"x",
        ))]);
        assert!(matches!(
            classify_shed_other(no_config, channel_id),
            ChannelUpdateTx::Custom(_)
        ));
    }

    /// A mixed tx (a `ChannelConfig` bundled with another channel op) stays
    /// `Custom`, but its config is retained in `config_entries`, and once mined
    /// `config_tip_at` resolves it — configs inside custom txs still advance
    /// the config lineage.
    #[test]
    fn mixed_config_tx_is_custom_but_advances_the_config_tip() {
        let channel_id = ChannelId::from([0; 32]);

        let config = channel_config(channel_id, MsgId::root());
        let config_id = config.id();
        let tx = unverified_tx_with_ops(vec![
            Op::ChannelConfig(config),
            Op::ChannelInscribe(inscribe_op(channel_id, MsgId::root(), b"m")),
        ]);
        let tx_hash = tx.hash();

        // Classification keeps the config in `config_entries`.
        let classified = classify_channel_txs(std::slice::from_ref(&tx), channel_id);
        assert_eq!(classified.len(), 1);
        match &classified[0] {
            BlockChannelTx::Custom { config_entries, .. } => {
                assert_eq!(config_entries.len(), 1);
                assert_eq!(config_entries[0].this_msg, config_id);
            }
            other => panic!("expected Custom carrying a config entry, got {other:?}"),
        }

        // Once mined, the config-tip walk resolves the config the mixed tx
        // carried — it is not lost to the `Custom` shape.
        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        state.process_block(
            block,
            genesis,
            genesis,
            vec![tx_hash],
            classified,
            Vec::new(),
        );
        assert_eq!(state.config_tip_at(block), config_id);
    }

    #[test]
    fn multi_inscribe_tx_advances_tip_and_delivers_adopted_entries() {
        // A valid tx can chain several inscriptions internally (each parents
        // the previous). It cannot be mirrored for retry, but every payload
        // that advanced the canonical tip must still reach the consumer via
        // `adopted` — otherwise consumer state falls behind the reported
        // `new_channel_tip` until finalization.
        let channel_id = ChannelId::from([0; 32]);
        let first = inscribe_op(channel_id, MsgId::root(), b"first");
        let second = inscribe_op(channel_id, first.id(), b"second");
        let (first_msg, second_msg) = (first.id(), second.id());
        let tx = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(first),
            Op::ChannelInscribe(second),
        ]);
        let tx_hash = tx.hash();

        let classified = classify_channel_txs(std::slice::from_ref(&tx), channel_id);
        assert_eq!(classified.len(), 1);
        assert!(
            matches!(&classified[0], BlockChannelTx::Custom { message_entries, .. } if message_entries.len() == 2)
        );

        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        let old_lineage = state.channel_lineage(genesis);
        mirror_channel_txs(
            &mut state,
            &classified,
            std::slice::from_ref(&tx),
            channel_id,
        );
        state.process_block(
            block,
            genesis,
            genesis,
            vec![tx_hash],
            classified,
            Vec::new(),
        );

        assert!(
            state.is_tracked(&tx_hash),
            "multi-inscribe is mirrored for retry"
        );
        assert_eq!(state.tx_source(&tx_hash), TxSource::Other);
        let update = state
            .detect_channel_update(&old_lineage, block, &HashSet::new())
            .expect("update");
        assert_eq!(update.new_channel_tip, second_msg, "tip stays ledger-true");
        // The tx is reported once, whole; its payloads are recoverable via
        // the public helper.
        assert_eq!(update.adopted.len(), 1);
        match &update.adopted[0] {
            ChannelUpdateTx::Custom(adopted_tx) => {
                let adopted_msgs: Vec<MsgId> = channel_inscriptions(adopted_tx, channel_id)
                    .iter()
                    .map(|i| i.this_msg)
                    .collect();
                assert_eq!(
                    adopted_msgs,
                    vec![first_msg, second_msg],
                    "every tip-advancing payload is delivered"
                );
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn adopted_bundle_carries_its_withdraws() {
        // An inscription+withdraw bundle observed on chain surfaces in
        // `adopted` as a typed bundle, withdraws included.
        let channel_id = ChannelId::from([0; 32]);
        let inscribe = inscribe_op(channel_id, MsgId::root(), b"bundle");
        let msg_id = inscribe.id();
        let withdrawn_note = NoteId::from(Fr::from(3u64));
        let withdraw = ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([withdrawn_note]),
        };
        let tx = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(inscribe),
            Op::ChannelWithdraw(withdraw),
        ]);
        let tx_hash = tx.hash();

        let classified = classify_channel_txs(std::slice::from_ref(&tx), channel_id);
        assert!(matches!(classified[0], BlockChannelTx::AtomicWithdraw(_)));

        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());
        let old_lineage = state.channel_lineage(genesis);
        mirror_channel_txs(
            &mut state,
            &classified,
            std::slice::from_ref(&tx),
            channel_id,
        );
        state.process_block(
            block,
            genesis,
            genesis,
            vec![tx_hash],
            classified,
            Vec::new(),
        );

        let update = state
            .detect_channel_update(&old_lineage, block, &HashSet::new())
            .expect("update");
        assert_eq!(update.adopted.len(), 1);
        match &update.adopted[0] {
            ChannelUpdateTx::AtomicWithdraw(a) => {
                assert_eq!(a.inscription.this_msg, msg_id);
                assert_eq!(a.withdraws.len(), 1);
                assert_eq!(a.withdraws[0].op.inputs, Inputs::new([withdrawn_note]));
            }
            other => panic!("expected AtomicWithdraw, got {other:?}"),
        }
    }

    #[test]
    fn extract_finalized_items_surfaces_standalone_withdraw() {
        // A ChannelWithdraw not bundled with an inscription (e.g. from
        // another sequencer or future multi-sig) should still surface as
        // a FinalizedOp::Withdraw — the sequencer stream is the complete
        // finalized view, not a "what we tracked locally" view.
        let channel_id = ChannelId::from([0; 32]);
        let other_channel = ChannelId::from([9; 32]);
        let withdraw_for_us = ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([NoteId::from(Fr::from(7u64))]),
        };
        let withdraw_other = ChannelWithdrawOp {
            channel_id: other_channel,
            inputs: Inputs::new([NoteId::from(Fr::from(0u64))]),
        };

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelWithdraw(withdraw_for_us),
            Op::ChannelWithdraw(withdraw_other),
        ]);
        let tx_hash = tx.hash();

        let items = extract_finalized_items(
            std::slice::from_ref(&tx),
            channel_id,
            Slot::from(7),
            &HashMap::new(),
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tx_hash, tx_hash);
        assert_eq!(items[0].l1_slot, Slot::from(7));
        assert_eq!(items[0].ops.len(), 1, "only our channel's withdraw");
        match &items[0].ops[0] {
            FinalizedOp::Withdraw(w) => {
                assert_eq!(w.tx_hash, tx_hash);
                assert_eq!(w.op.channel_id, channel_id);
                assert_eq!(w.op.inputs, Inputs::new([NoteId::from(Fr::from(7u64))]));
            }
            other => panic!("expected Withdraw, got {other:?}"),
        }
    }

    fn channel_config(channel_id: ChannelId, parent: MsgId) -> ChannelConfigOp {
        let signer = Ed25519Key::from_bytes(&[0u8; 32]).public_key();
        ChannelConfigOp {
            channel: channel_id,
            parent,
            keys: VerifiedChannelKeys::try_from(vec![signer]).unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        }
    }

    fn dummy_pending_tx(seed: u8) -> SignedOps<Unverified, StandardMode> {
        let mantle_tx = Ops::from([Op::ChannelInscribe(InscriptionOp {
            channel_id: [0u8; 32].into(),
            inscription: Inscription::new_unchecked(vec![seed]),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[seed; 32])
                .public_key()
                .into_unverified(),
        })]);
        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(Ed25519Signature::zero())]);
        SignedOps::from_parts(mantle_tx, op_proofs)
            .expect("Should generate a valid transaction with valid matching proofs.")
    }

    /// Run a synchronous callable on a background thread and bail out if it
    /// doesn't return within `timeout`. Used so a non-terminating lineage
    /// walk surfaces as a clear test failure rather than hanging CI.
    fn run_with_timeout<R: Send + 'static>(
        timeout: std::time::Duration,
        f: impl FnOnce() -> R + Send + 'static,
    ) -> R {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(tx.send(f()));
        });
        rx.recv_timeout(timeout)
            .expect("extraction hung (suspected lineage cycle in classify_channel_txs)")
    }

    #[test]
    fn config_in_block_does_not_advance_channel_tip() {
        // Block layout: [Inscribe I1 (parent=root), ChannelConfig X
        //                (parent=root), Inscribe I2 (parent=I1)]. The config
        //                moves only the config lineage, so I2 chains off I1
        //                and the block's channel tip is I2.
        let channel_id = ChannelId::from([0u8; 32]);
        let config = channel_config(channel_id, MsgId::root());
        let i1 = inscribe_op(channel_id, MsgId::root(), b"i1");
        let i1_id = i1.id();
        let i2 = inscribe_op(channel_id, i1_id, b"i2");
        let i2_id = i2.id();

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(i1),
            Op::ChannelConfig(config),
            Op::ChannelInscribe(i2),
        ]);
        let tx_hash = tx.hash();

        // Stage pending inscriptions BEFORE driving the block through.
        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        // Pending chained from I1 — its position is taken by the mined I2.
        let pending_stale = dummy_pending_tx(1);
        let pending_stale_hash = pending_stale.hash();
        state
            .submit_inscription(
                pending_stale,
                i1_id,
                MsgId::from([99u8; 32]),
                Inscription::new_unchecked(b"chained-from-i1".to_vec()),
            )
            .unwrap();

        // Pending chained from the block tip — should remain on-branch.
        let pending_live = dummy_pending_tx(2);
        let pending_live_hash = pending_live.hash();
        state
            .submit_inscription(
                pending_live,
                i2_id,
                MsgId::from([88u8; 32]),
                Inscription::new_unchecked(b"chained-from-i2".to_vec()),
            )
            .unwrap();

        let extracted = run_with_timeout(std::time::Duration::from_secs(2), move || {
            classify_channel_txs(std::slice::from_ref(&tx), channel_id)
        });
        state.process_block(
            block,
            genesis,
            genesis,
            vec![tx_hash],
            extracted,
            Vec::new(),
        );

        assert_eq!(
            state.channel_tip_at(block),
            i2_id,
            "the config must not advance the channel tip"
        );

        // shed_off_branch_pending should drop the stale one but not the live one.
        let shed = state.shed_off_branch_pending(block);
        let shed_hashes: HashSet<TxHash> = shed.iter().map(PendingTx::tx_hash).collect();
        assert!(
            shed_hashes.contains(&pending_stale_hash),
            "pending chained from I1 lost its position to the mined I2"
        );
        assert!(
            !shed_hashes.contains(&pending_live_hash),
            "pending chained from the block tip must remain on-branch"
        );
    }

    #[test]
    fn extract_finalized_items_surfaces_configs_with_their_own_parent() {
        let channel_id = ChannelId::from([0u8; 32]);
        let parent = MsgId::from([7u8; 32]);
        let config = channel_config(channel_id, parent);
        let config_id = config.id();
        let tx = unverified_tx_with_ops(vec![Op::ChannelConfig(config)]);
        let tx_hash = tx.hash();

        let items = extract_finalized_items(
            std::slice::from_ref(&tx),
            channel_id,
            Slot::from(7),
            &HashMap::new(),
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].ops.len(), 1);
        match &items[0].ops[0] {
            FinalizedOp::Config(info) => {
                assert_eq!(info.tx_hash, tx_hash);
                assert_eq!(info.parent_msg, parent);
                assert_eq!(info.this_msg, config_id);
                assert!(info.payload.is_empty());
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn canonical_backfill_gap_inscriptions_surface_as_adopted() {
        // A sequencer whose block stream skipped a block self-heals via
        // backfill_canonical. Inscriptions mined in the gap block are new to
        // this sequencer's canonical view, so they must be reported as
        // `adopted` — otherwise no consumer learns they landed until
        // finalization.
        //
        // Chain: G(0) <- B1 <- B2 <- B3
        //   B1 carries inscription A (parent = root), delivered live
        //   B2 carries inscription Y (parent = A), MISSED by the stream
        //   B3 is empty, delivered live with B2's parent missing
        let channel_id = ChannelId::from([0u8; 32]);

        let a = inscribe_op(channel_id, MsgId::root(), b"a");
        let a_id = a.id();
        let y = inscribe_op(channel_id, a_id, b"y");
        let y_id = y.id();

        let b1 = api_block(
            1,
            0,
            1,
            vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(a)])],
        );
        let b2 = api_block(
            2,
            1,
            2,
            vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(y)])],
        );
        let b3 = api_block(3, 2, 3, Vec::new());

        let node = MockNode {
            blocks: vec![b2],
            ..MockNode::default()
        };
        let mut state = None;
        let mut current_tip = None;
        let mut lib_slot = Slot::genesis();

        // First live event: B1 arrives normally and adopts A.
        let first = handle_block_event(
            &live_event(&b1),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B1 succeeds");
        let update = first.channel_update.expect("B1 adopts inscription A");
        assert!(
            update
                .adopted
                .iter()
                .any(|t| t.inscription().is_some_and(|i| i.this_msg == a_id)),
            "sanity: A is adopted on the first event"
        );

        // Second live event: B3 arrives with its parent B2 missing, so the
        // canonical backfill fetches B2 (carrying Y). Y is newly canonical
        // from this sequencer's perspective and was never surfaced before,
        // so this event's channel update must report it as adopted.
        let second = handle_block_event(
            &live_event(&b3),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B3 succeeds");
        let update = second
            .channel_update
            .expect("the backfilled gap advanced the channel tip; expected a channel update");
        assert!(
            update
                .adopted
                .iter()
                .any(|t| t.inscription().is_some_and(|i| i.this_msg == y_id)),
            "inscription mined in the backfilled gap block must be reported as adopted; \
             got adopted={:?}, orphaned={:?}",
            update.adopted,
            update.orphaned,
        );
    }

    // ---- channel-view scenarios: chain diagram + what the consumer sees ----

    /// An inscription tx: `(msg id, tx)`.
    fn ins(
        channel_id: ChannelId,
        parent: MsgId,
        payload: &[u8],
    ) -> (MsgId, SignedOps<Unverified, StandardMode>) {
        let op = inscribe_op(channel_id, parent, payload);
        let id = op.id();
        (id, unverified_tx_with_ops(vec![Op::ChannelInscribe(op)]))
    }

    /// A fork block event: `block` processed, tip unchanged at `tip`.
    fn fork_event(block: &ApiBlock, tip: &ApiBlock) -> ProcessedBlockEvent {
        ProcessedBlockEvent {
            block: block.clone(),
            tip: tip.header.id,
            tip_slot: tip.header.slot,
            lib: header_id(0),
            lib_slot: Slot::genesis(),
        }
    }

    /// One driven event: the handler's result and what the sheds drained.
    struct Driven {
        result: BlockEventResult,
        shed: Vec<PendingTx>,
        shed_other: Vec<SignedOps<Unverified, StandardMode>>,
    }

    /// Run `events` as the actor would: handle each, then the shed passes.
    async fn drive(
        state: &mut Option<TxState>,
        channel_id: ChannelId,
        events: &[ProcessedBlockEvent],
    ) -> Vec<Driven> {
        drive_with(&MockNode::default(), state, channel_id, events).await
    }

    async fn drive_with(
        node: &MockNode,
        state: &mut Option<TxState>,
        channel_id: ChannelId,
        events: &[ProcessedBlockEvent],
    ) -> Vec<Driven> {
        let mut current_tip = None;
        let mut lib_slot = Slot::genesis();
        let mut driven = Vec::with_capacity(events.len());
        for event in events {
            let result = handle_block_event(
                event,
                state,
                &mut current_tip,
                &mut lib_slot,
                channel_id,
                node,
            )
            .await
            .unwrap();
            let s = state.as_mut().unwrap();
            let tip = current_tip.unwrap();
            let mut shed = s.shed_bundles_with_missing_inputs(tip, channel_id);
            shed.extend(s.shed_off_branch_pending(tip));
            let shed_other = s.shed_off_branch_pending_other(tip);
            driven.push(Driven {
                result,
                shed,
                shed_other,
            });
        }
        driven
    }

    fn msg_ids(txs: &[ChannelUpdateTx]) -> Vec<MsgId> {
        txs.iter()
            .filter_map(|t| t.inscription().map(|i| i.this_msg))
            .collect()
    }

    /// LEZ "missing 12375" (2026-09-13): a fork block's inscription is not in
    /// the view until its branch is canonical; then it is adopted.
    #[tokio::test]
    async fn fork_block_inscription_is_adopted_on_the_switch() {
        // G <- F(a) canonical; G <- C(a, y) fork; C <- E empty: C wins.
        let ch = ChannelId::from([0u8; 32]);
        let (a_id, a_tx) = ins(ch, MsgId::root(), b"a");
        let (y_id, y_tx) = ins(ch, a_id, b"y");
        let bf = api_block(1, 0, 1, vec![a_tx.clone()]);
        let bc = api_block(2, 0, 2, vec![a_tx, y_tx]);
        let be = api_block(3, 2, 3, Vec::new());

        let mut state = None;
        let r = drive(
            &mut state,
            ch,
            &[live_event(&bf), fork_event(&bc, &bf), live_event(&be)],
        )
        .await;

        assert!(
            r[0].result.channel_update.is_some(),
            "a adopted on the first event"
        );
        assert!(
            r[1].result.channel_update.is_none(),
            "fork block: not in the view"
        );
        let u = r[2]
            .result
            .channel_update
            .as_ref()
            .expect("y canonical on the switch");
        assert_eq!(msg_ids(&u.adopted), vec![y_id]);
        assert!(u.orphaned.is_empty());
        assert_eq!(u.new_channel_tip, y_id);
        // The re-mirrored y is mined on the new branch: the shed must keep it,
        // and it must not be offered for resubmission.
        assert!(
            r[2].shed.is_empty(),
            "re-mirrored fork tx shed as off-branch: {:?}",
            r[2].shed
        );
        let s = state.as_ref().unwrap();
        assert_eq!(s.pending_publish_count(), 2, "a and y mirrored");
        assert!(
            s.pending_txs(be.header.id).is_empty(),
            "nothing to resubmit: both are safe at the tip"
        );
    }

    /// Pending suffix vs. forks: a bare un-mine keeps it, a fork competitor
    /// changes nothing, the fork winning replaces it in one report.
    #[tokio::test]
    async fn pending_suffix_survives_forks_until_a_competitor_is_canonical() {
        // G <- B1(i1, i2) <- B2(i3, i4); B1 <- B3 empty (tip: i3, i4 pending);
        // B1 <- B4(i3') fork; B4 <- B5(i4') canonical: B4's branch wins.
        let ch = ChannelId::from([0u8; 32]);
        let (i1_id, i1_tx) = ins(ch, MsgId::root(), b"i1");
        let (i2_id, i2_tx) = ins(ch, i1_id, b"i2");
        let (i3_id, i3_tx) = ins(ch, i2_id, b"i3");
        let (i4_id, i4_tx) = ins(ch, i3_id, b"i4");
        let (i3a_id, i3a_tx) = ins(ch, i2_id, b"i3'");
        let (i4a_id, i4a_tx) = ins(ch, i3a_id, b"i4'");
        let (i3_hash, i4_hash) = (i3_tx.hash(), i4_tx.hash());
        let b1 = api_block(1, 0, 1, vec![i1_tx, i2_tx]);
        let b2 = api_block(2, 1, 2, vec![i3_tx, i4_tx]);
        let b3 = api_block(3, 1, 3, Vec::new());
        let b4 = api_block(4, 1, 4, vec![i3a_tx]);
        let b5 = api_block(5, 4, 5, vec![i4a_tx]);

        let mut state = None;
        let r = drive(
            &mut state,
            ch,
            &[
                live_event(&b1),
                live_event(&b2),
                live_event(&b3),
                fork_event(&b4, &b3),
                live_event(&b5),
            ],
        )
        .await;

        assert!(
            r[2].result.channel_update.is_none(),
            "bare un-mine keeps i3, i4"
        );
        assert!(
            r[3].result.channel_update.is_none(),
            "fork competitor: nothing decided"
        );
        let u = r[4].result.channel_update.as_ref().expect("the switch");
        assert_eq!(msg_ids(&u.orphaned), vec![i3_id, i4_id]);
        assert_eq!(msg_ids(&u.adopted), vec![i3a_id, i4a_id]);
        assert_eq!(u.new_channel_tip, i4a_id);
        // Only the displaced suffix is shed; the re-mirrored i3', i4' read as
        // mined on the new branch.
        assert!(r[2].shed.is_empty() && r[3].shed.is_empty());
        let shed: Vec<TxHash> = r[4].shed.iter().map(PendingTx::tx_hash).collect();
        assert_eq!(shed, vec![i3_hash, i4_hash]);
        let s = state.as_ref().unwrap();
        assert_eq!(s.pending_publish_count(), 4, "i1, i2, i3', i4' mirrored");
        assert!(s.pending_txs(b5.header.id).is_empty());
    }

    /// Our opaque (custom-shaped) pending tx is part of the view like any
    /// pending inscription: a bare un-mine keeps it, a mined competitor
    /// displaces it, reported whole as `Custom`.
    #[tokio::test]
    async fn opaque_pending_tx_stays_in_the_view_across_a_bare_unmine() {
        // G <- B0 empty; B0 <- B1(custom [x1, x2]); B0 <- B2 empty (tip);
        // B2 <- B3(y, parent root).
        let ch = ChannelId::from([0u8; 32]);
        let x1 = inscribe_op(ch, MsgId::root(), b"x1");
        let x2 = inscribe_op(ch, x1.id(), b"x2");
        let (x1_id, x2_id) = (x1.id(), x2.id());
        let custom = unverified_tx_with_ops(vec![Op::ChannelInscribe(x1), Op::ChannelInscribe(x2)]);
        let custom_hash = custom.hash();
        let (y_id, y_tx) = ins(ch, MsgId::root(), b"y");
        let b0 = api_block(1, 0, 1, Vec::new());
        let b1 = api_block(2, 1, 2, vec![custom.clone()]);
        let b2 = api_block(3, 1, 3, Vec::new());
        let b3 = api_block(4, 3, 4, vec![y_tx]);

        let mut tx_state = TxState::new(header_id(0), MsgId::root());
        tx_state.submit_other(custom, ch).unwrap();
        let mut state = Some(tx_state);
        let r = drive(
            &mut state,
            ch,
            &[
                live_event(&b0),
                live_event(&b1),
                live_event(&b2),
                live_event(&b3),
            ],
        )
        .await;

        assert!(
            r[1].result
                .channel_update
                .as_ref()
                .is_none_or(|u| u.adopted.is_empty() && u.orphaned.is_empty()),
            "own pending landing is not a view change"
        );
        assert!(
            r[2].result.channel_update.is_none(),
            "bare un-mine keeps it"
        );
        let u = r[3].result.channel_update.as_ref().expect("y displaced it");
        let [ChannelUpdateTx::Custom(orphaned_tx)] = &u.orphaned[..] else {
            panic!("one Custom orphan, got {:?}", u.orphaned);
        };
        let orphaned: Vec<MsgId> = channel_inscriptions(orphaned_tx, ch)
            .iter()
            .map(|i| i.this_msg)
            .collect();
        assert_eq!(orphaned, vec![x1_id, x2_id]);
        assert_eq!(msg_ids(&u.adopted), vec![y_id]);
        assert!(r[1].shed_other.is_empty() && r[2].shed_other.is_empty());
        let shed: Vec<TxHash> = r[3].shed_other.iter().map(SignedOps::hash).collect();
        assert_eq!(shed, vec![custom_hash]);
    }

    /// A deposit re-created as `recreated`, our identity pin bundle spending it
    /// (pending, own), and a node serving the deposit event for block 1.
    struct PinFixture {
        dep_tx: SignedOps<Unverified, StandardMode>,
        pin_tx: SignedOps<Unverified, StandardMode>,
        pin_id: MsgId,
        recreated: NoteId,
        node: MockNode,
        state: TxState,
    }

    fn pin_fixture(ch: ChannelId) -> PinFixture {
        let recreated = NoteId::from(Fr::from(1010u64));
        let pk = lb_key_management_system_service::keys::ZkPublicKey::from(Fr::from(7u64));
        let dep = deposit_op(ch, 1, Metadata::try_from(b"d".to_vec()).unwrap());
        let dep_tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(dep.clone())]);
        let pin_op = inscribe_op(ch, MsgId::root(), b"pin");
        let pin_id = pin_op.id();
        let pin_tx = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(pin_op.clone()),
            Op::ChannelTransfer(ChannelTransferOp {
                channel_id: ch,
                inputs: Inputs::new([recreated]),
                outputs: Outputs::new([Note::new(50, pk)]),
            }),
        ]);
        let note = DepositNote {
            note_id: recreated,
            value: 50,
            pk,
        };
        let node = MockNode {
            events: HashMap::from([(header_id(1), deposit_event(&dep_tx, &dep, 50, vec![note]))]),
            ..MockNode::default()
        };
        let mut state = TxState::new(header_id(0), MsgId::root());
        state
            .submit_pin_deposit(
                pin_tx.clone(),
                MsgId::root(),
                pin_id,
                pin_op.inscription,
                Inputs::new([recreated]),
            )
            .unwrap();
        PinFixture {
            dep_tx,
            pin_tx,
            pin_id,
            recreated,
            node,
            state,
        }
    }

    /// A reorg dropping the deposit with the pin leaves the pin unlandable:
    /// shed it and its pending child, parent first.
    #[tokio::test]
    async fn bundle_whose_inputs_left_the_branch_is_shed_with_its_children() {
        // G <- B1(deposit, pin) canonical; G <- B2 empty sibling: B2 wins.
        let ch = ChannelId::from([0u8; 32]);
        let f = pin_fixture(ch);
        let (x_id, x_tx) = ins(ch, f.pin_id, b"x");
        let (pin_hash, x_hash) = (f.pin_tx.hash(), x_tx.hash());
        let b1 = api_block(1, 0, 1, vec![f.dep_tx.clone(), f.pin_tx.clone()]);
        let b2 = api_block(2, 0, 2, Vec::new());

        let mut state = f.state;
        state
            .submit_inscription(
                x_tx,
                f.pin_id,
                x_id,
                Inscription::new_unchecked(b"x".to_vec()),
            )
            .unwrap();
        let mut state = Some(state);
        let r = drive_with(&f.node, &mut state, ch, &[live_event(&b1), live_event(&b2)]).await;

        assert!(r[0].shed.is_empty());
        let shed: Vec<TxHash> = r[1].shed.iter().map(PendingTx::tx_hash).collect();
        assert_eq!(shed, vec![pin_hash, x_hash], "pin first, then its child");
        assert!(matches!(r[1].shed[0], PendingTx::PinDeposit(_)));
        assert!(matches!(r[1].shed[1], PendingTx::Inscription(_)));
        assert_eq!(state.as_ref().unwrap().pending_publish_count(), 0);
    }

    /// A custom tx (two chained inscriptions): `(last msg id, tx)`.
    fn custom(
        channel_id: ChannelId,
        parent: MsgId,
        payload: &[u8],
    ) -> (MsgId, SignedOps<Unverified, StandardMode>) {
        let first = inscribe_op(channel_id, parent, payload);
        let second = inscribe_op(channel_id, first.id(), payload);
        let id = second.id();
        let ops = vec![Op::ChannelInscribe(first), Op::ChannelInscribe(second)];
        (id, unverified_tx_with_ops(ops))
    }

    /// A custom tx is mirrored like any channel tx: a bare un-mine keeps it
    /// in the view, silent; a competitor on its parent displaces it and it
    /// is reported orphaned.
    #[tokio::test]
    async fn foreign_custom_tx_is_kept_through_un_mine_and_orphaned_by_a_competitor() {
        // G <- A(c) tip; G <- X, X <- Y forks; Y <- Z tip: A un-mined bare;
        // Z <- R(i') tip: i' takes the parent c chained on.
        let ch = ChannelId::from([0u8; 32]);
        let (c_id, c_tx) = custom(ch, MsgId::root(), b"c");
        let (i_id, i_tx) = ins(ch, MsgId::root(), b"i'");
        let (c_hash, i_hash) = (c_tx.hash(), i_tx.hash());
        let ba = api_block(1, 0, 1, vec![c_tx]);
        let bx = api_block(2, 0, 2, Vec::new());
        let by = api_block(3, 2, 3, Vec::new());
        let bz = api_block(4, 3, 4, Vec::new());
        let br = api_block(5, 4, 5, vec![i_tx]);

        let mut state = None;
        let r = drive(
            &mut state,
            ch,
            &[
                live_event(&ba),
                fork_event(&bx, &ba),
                fork_event(&by, &ba),
                live_event(&bz),
                live_event(&br),
            ],
        )
        .await;

        let u = r[0].result.channel_update.as_ref().expect("c is adopted");
        assert!(
            matches!(u.adopted.as_slice(), [ChannelUpdateTx::Custom(tx)] if tx.hash() == c_hash)
        );
        assert_eq!(u.new_channel_tip, c_id);
        assert!(
            r[3].result.channel_update.is_none(),
            "bare un-mine is silent"
        );
        assert!(r[3].shed_other.is_empty());
        let u = r[4].result.channel_update.as_ref().expect("i' wins");
        let orphaned: Vec<TxHash> = u.orphaned.iter().map(ChannelUpdateTx::tx_hash).collect();
        let adopted: Vec<TxHash> = u.adopted.iter().map(ChannelUpdateTx::tx_hash).collect();
        assert_eq!(orphaned, vec![c_hash]);
        assert_eq!(adopted, vec![i_hash]);
        assert_eq!(u.new_channel_tip, i_id);
        let shed: Vec<TxHash> = r[4].shed_other.iter().map(SignedOps::hash).collect();
        assert_eq!(shed, vec![c_hash]);
    }

    /// A config is reported like any channel tx: adopted when mined, kept
    /// silently through a bare un-mine, orphaned when a rival supersedes it.
    #[tokio::test]
    async fn foreign_config_is_adopted_kept_through_un_mine_and_orphaned_by_a_rival() {
        // G <- A(cfg) tip; G <- X, X <- Y forks; Y <- Z tip: A un-mined bare;
        // Z <- R(cfg') tip: cfg' takes the config slot cfg chained on.
        let ch = ChannelId::from([0u8; 32]);
        let cfg =
            unverified_tx_with_ops(vec![Op::ChannelConfig(channel_config(ch, MsgId::root()))]);
        let mut rival_op = channel_config(ch, MsgId::root());
        rival_op.posting_timeframe = SlotTimeframe::from(1u32);
        let rival = unverified_tx_with_ops(vec![Op::ChannelConfig(rival_op)]);
        let (cfg_hash, rival_hash) = (cfg.hash(), rival.hash());
        let ba = api_block(1, 0, 1, vec![cfg]);
        let bx = api_block(2, 0, 2, Vec::new());
        let by = api_block(3, 2, 3, Vec::new());
        let bz = api_block(4, 3, 4, Vec::new());
        let br = api_block(5, 4, 5, vec![rival]);

        let mut state = None;
        let r = drive(
            &mut state,
            ch,
            &[
                live_event(&ba),
                fork_event(&bx, &ba),
                fork_event(&by, &ba),
                live_event(&bz),
                live_event(&br),
            ],
        )
        .await;

        let u = r[0].result.channel_update.as_ref().expect("cfg is adopted");
        assert!(u.orphaned.is_empty());
        assert!(
            matches!(u.adopted.as_slice(), [ChannelUpdateTx::Config(tx)] if tx.hash() == cfg_hash)
        );
        assert_eq!(
            u.new_channel_tip,
            MsgId::root(),
            "configs never move the message tip"
        );
        assert!(
            r[3].result.channel_update.is_none(),
            "bare un-mine is silent"
        );
        let s = state.as_ref().unwrap();
        assert!(s.is_tracked(&cfg_hash));
        assert!(
            s.pending_txs(bz.header.id)
                .iter()
                .any(|(h, _)| *h == cfg_hash),
            "un-mined, it is re-posted"
        );
        let u = r[4]
            .result
            .channel_update
            .as_ref()
            .expect("cfg' supersedes cfg");
        let orphaned: Vec<TxHash> = u.orphaned.iter().map(ChannelUpdateTx::tx_hash).collect();
        let adopted: Vec<TxHash> = u.adopted.iter().map(ChannelUpdateTx::tx_hash).collect();
        assert_eq!(orphaned, vec![cfg_hash]);
        assert_eq!(adopted, vec![rival_hash]);
    }

    /// After a checkpoint restore the first event's view carries the restored
    /// pending publish, which `adopted` never echoes.
    #[tokio::test]
    async fn first_event_after_restore_keeps_restored_pending_in_the_prefix() {
        let ch = ChannelId::from([0u8; 32]);
        let (p_id, p_tx) = ins(ch, MsgId::root(), b"p");
        let p_hash = p_tx.hash();
        let mut state = TxState::new(header_id(0), MsgId::root());
        state
            .submit_inscription(
                p_tx,
                MsgId::root(),
                p_id,
                Inscription::new_unchecked(b"p".to_vec()),
            )
            .unwrap();
        let b1 = api_block(1, 0, 1, Vec::new());

        let r = drive(&mut Some(state), ch, &[live_event(&b1)]).await;

        let prefix: Vec<TxHash> = r[0]
            .result
            .common_prefix
            .iter()
            .map(ChannelUpdateTx::tx_hash)
            .collect();
        assert_eq!(prefix, vec![p_hash]);
        assert!(
            r[0].result
                .channel_update
                .as_ref()
                .is_none_or(|u| u.adopted.is_empty())
        );
    }

    /// `common_prefix ++ adopted` keeps each lineage in order; the message and
    /// config lineages are not interleaved with each other.
    #[tokio::test]
    async fn view_split_keeps_each_lineage_in_order() {
        // Pending config C in the view; B1 adopts M1 <- M2.
        let ch = ChannelId::from([0u8; 32]);
        let cfg =
            unverified_tx_with_ops(vec![Op::ChannelConfig(channel_config(ch, MsgId::root()))]);
        let cfg_hash = cfg.hash();
        let mut state = TxState::new(header_id(0), MsgId::root());
        state.submit_other(cfg, ch).unwrap();
        let (m1_id, m1) = ins(ch, MsgId::root(), b"m1");
        let (m2_id, m2) = ins(ch, m1_id, b"m2");
        let b1 = api_block(1, 0, 1, vec![m1, m2]);

        let r = drive(&mut Some(state), ch, &[live_event(&b1)]).await;

        let prefix: Vec<TxHash> = r[0]
            .result
            .common_prefix
            .iter()
            .map(ChannelUpdateTx::tx_hash)
            .collect();
        assert_eq!(prefix, vec![cfg_hash]);
        let u = r[0].result.channel_update.as_ref().expect("M1, M2 adopted");
        assert_eq!(msg_ids(&u.adopted), vec![m1_id, m2_id]);
    }

    /// After a restore, a pending P whose unfinalized parent M is rediscovered
    /// on the first event: P proves M was the view before the restart, so
    /// nothing is adopted, shed or orphaned; the view is M <- P, and P lands
    /// next.
    #[tokio::test]
    async fn restored_pending_survives_its_parent_being_rediscovered() {
        let ch = ChannelId::from([0u8; 32]);
        let (m_id, m_tx) = ins(ch, MsgId::root(), b"m");
        let (p_id, p_tx) = ins(ch, m_id, b"p");
        let (m_hash, p_hash) = (m_tx.hash(), p_tx.hash());
        let mut restored = TxState::new(header_id(0), MsgId::root());
        restored
            .submit_inscription(
                p_tx.clone(),
                m_id,
                p_id,
                Inscription::new_unchecked(b"p".to_vec()),
            )
            .unwrap();
        let b1 = api_block(1, 0, 1, vec![m_tx]);
        let b2 = api_block(2, 1, 2, vec![p_tx]);

        let r = drive(&mut Some(restored), ch, &[live_event(&b1), live_event(&b2)]).await;

        assert!(r[0].shed.is_empty(), "P chains on M, so it stays pending");
        assert!(
            r[0].result.channel_update.is_none(),
            "M was the view P chained on: nothing changed"
        );
        assert_eq!(
            hashes(&r[0].result.common_prefix),
            vec![m_hash, p_hash],
            "the view in lineage order"
        );

        // P lands on its parent: an own publish is not echoed as adopted, so
        // no update; the view is now M <- P, both mined.
        assert!(r[1].shed.is_empty());
        assert!(
            r[1].result.channel_update.is_none(),
            "nothing new to report"
        );
        assert_eq!(hashes(&r[1].result.common_prefix), vec![m_hash, p_hash]);
    }

    /// After a restore, a pending P whose parent M is never mined is shed the
    /// moment a rival takes M's slot: a conflict, P is orphaned and not in
    /// the prefix.
    #[tokio::test]
    async fn restored_pending_is_orphaned_when_a_rival_takes_its_parents_slot() {
        let ch = ChannelId::from([0u8; 32]);
        let (m_id, _m_tx) = ins(ch, MsgId::root(), b"m");
        let (p_id, p_tx) = ins(ch, m_id, b"p");
        let (_rival_id, rival_tx) = ins(ch, MsgId::root(), b"rival");
        let (p_hash, rival_hash) = (p_tx.hash(), rival_tx.hash());
        let mut restored = TxState::new(header_id(0), MsgId::root());
        restored
            .submit_inscription(p_tx, m_id, p_id, Inscription::new_unchecked(b"p".to_vec()))
            .unwrap();
        let b1 = api_block(1, 0, 1, vec![rival_tx]);

        let r = drive(&mut Some(restored), ch, &[live_event(&b1)]).await;

        // The actor reports the shed as orphaned (`build_channel_update`).
        let shed: Vec<ChannelUpdateTx> = r[0]
            .shed
            .clone()
            .into_iter()
            .map(orphan_from_shed)
            .collect();
        assert_eq!(hashes(&shed), vec![p_hash], "P no longer chains on the tip");
        let u = r[0]
            .result
            .channel_update
            .as_ref()
            .expect("rival is adopted");
        assert!(u.orphaned.is_empty(), "nothing mined was dropped");
        assert_eq!(hashes(&u.adopted), vec![rival_hash]);
        assert!(r[0].result.common_prefix.is_empty());
    }

    fn hashes(txs: &[ChannelUpdateTx]) -> Vec<TxHash> {
        txs.iter().map(ChannelUpdateTx::tx_hash).collect()
    }

    /// On an extension the new view is the previous view followed by
    /// `adopted`: across a checkpoint restore with a pending publish P on the
    /// finalized tip M, an empty block, the block that mines P (which
    /// `adopted` never echoes), and a block that mines N on top.
    #[tokio::test]
    async fn extension_appends_adopted_to_the_previous_view() {
        let ch = ChannelId::from([0u8; 32]);
        let (m_id, _) = ins(ch, MsgId::root(), b"m");
        let (p_id, p_tx) = ins(ch, m_id, b"p");
        let (_, n_tx) = ins(ch, p_id, b"n");
        let mut restored = TxState::new(header_id(0), m_id);
        restored
            .submit_inscription(
                p_tx.clone(),
                m_id,
                p_id,
                Inscription::new_unchecked(b"p".to_vec()),
            )
            .unwrap();
        let blocks = [
            api_block(1, 0, 1, Vec::new()),
            api_block(2, 1, 2, vec![p_tx]),
            api_block(3, 2, 3, vec![n_tx]),
        ];

        let node = MockNode::default();
        let mut state = Some(restored);
        let mut current_tip = None;
        let mut lib_slot = Slot::genesis();
        let mut view = hashes(
            &state
                .as_ref()
                .unwrap()
                .channel_view_txs(header_id(0), &HashSet::new()),
        );
        for block in &blocks {
            let result = handle_block_event(
                &live_event(block),
                &mut state,
                &mut current_tip,
                &mut lib_slot,
                ch,
                &node,
            )
            .await
            .unwrap();
            let adopted = result.channel_update.as_ref().map_or_else(Vec::new, |u| {
                assert!(u.orphaned.is_empty(), "an extension");
                hashes(&u.adopted)
            });
            let tip = current_tip.expect("processed");
            let new_view = hashes(
                &state
                    .as_ref()
                    .unwrap()
                    .channel_view_txs(tip, &HashSet::new()),
            );
            assert_eq!(new_view, [view.clone(), adopted].concat());
            view = new_view;
        }
        assert_eq!(view.len(), 2, "P then N");
    }

    /// A shed bundle the consumer was told to revert can still land: its bytes
    /// stay in the L1 mempool. When it does, on a pure extension, it must be
    /// reported adopted — the shed removed it from pending, so the consumer
    /// no longer holds it. "Ours" is not "already applied".
    #[tokio::test]
    async fn shed_bundle_that_lands_anyway_is_reported_adopted() {
        // G <- B1(deposit, pin) canonical; G <- B2 empty: B2 wins, pin shed;
        // B2 <- B3(deposit, pin): both re-mined from the mempool.
        let ch = ChannelId::from([0u8; 32]);
        let mut f = pin_fixture(ch);
        let pin_hash = f.pin_tx.hash();
        let b1 = api_block(1, 0, 1, vec![f.dep_tx.clone(), f.pin_tx.clone()]);
        let b2 = api_block(2, 0, 2, Vec::new());
        let b3 = api_block(3, 2, 3, vec![f.dep_tx.clone(), f.pin_tx.clone()]);
        let dep_event = f.node.events[&header_id(1)].clone();
        f.node.events.insert(header_id(3), dep_event);

        let mut state = Some(f.state);
        let r = drive_with(
            &f.node,
            &mut state,
            ch,
            &[live_event(&b1), live_event(&b2), live_event(&b3)],
        )
        .await;

        let shed: Vec<TxHash> = r[1].shed.iter().map(PendingTx::tx_hash).collect();
        assert_eq!(shed, vec![pin_hash], "sanity: the pin is shed when B2 wins");
        let u = r[2]
            .result
            .channel_update
            .as_ref()
            .expect("the pin landing moves the channel tip");
        assert!(u.orphaned.is_empty());
        let adopted: Vec<TxHash> = u.adopted.iter().map(ChannelUpdateTx::tx_hash).collect();
        assert_eq!(
            adopted,
            vec![pin_hash],
            "a shed own bundle that lands anyway must be reported adopted"
        );
        assert_eq!(u.new_channel_tip, f.pin_id);
        assert!(r[2].shed.is_empty(), "it is mined on this branch");
    }

    /// The pin un-mined alone stays pending and silent; its note is back in
    /// the wallet view but reserved for it, so selection must not offer it.
    #[tokio::test]
    async fn bundle_whose_inputs_stayed_on_the_branch_is_kept_and_its_notes_reserved() {
        // G <- B1(deposit) <- B2(pin) canonical; B1 <- B3 empty: B3 wins.
        let ch = ChannelId::from([0u8; 32]);
        let f = pin_fixture(ch);
        let b1 = api_block(1, 0, 1, vec![f.dep_tx.clone()]);
        let b2 = api_block(2, 1, 2, vec![f.pin_tx.clone()]);
        let b3 = api_block(3, 1, 3, Vec::new());

        let mut state = Some(f.state);
        let r = drive_with(
            &f.node,
            &mut state,
            ch,
            &[live_event(&b1), live_event(&b2), live_event(&b3)],
        )
        .await;

        assert!(r[2].shed.is_empty());
        assert!(
            r[2].result.channel_update.is_none(),
            "bare un-mine is silent"
        );
        let s = state.as_ref().unwrap();
        assert_eq!(s.pending_publish_count(), 1);
        let has = |v: &crate::sequencer::ChannelWalletView| {
            v.finalized
                .iter()
                .chain(v.unfinalized.iter())
                .any(|n| n.note_id == f.recreated)
        };
        assert!(has(&s.channel_wallet_view(Some(header_id(3)))));
        assert!(
            !has(&s.spendable_wallet_view(Some(header_id(3)), ch)),
            "reserved"
        );
    }

    /// Flip-flop reorg: B wins, loses to B', wins back. Its entry was shed on
    /// the loss and must be re-mirrored from the store on the win, so a later
    /// bare un-mine keeps it in the view. Also covers a fork block (B')
    /// becoming canonical.
    #[tokio::test]
    async fn flip_flop_reorg_re_mirrors_the_readopted_block() {
        // G <- A(i1); A <- B(i2); A <- B'(i2') fork; B' <- C' empty (tip);
        // B <- C fork; C <- D empty (tip); A <- X, X <- Y forks; Y <- Z empty (tip).
        let ch = ChannelId::from([0u8; 32]);
        let (i1_id, i1_tx) = ins(ch, MsgId::root(), b"i1");
        let (i2_id, i2_tx) = ins(ch, i1_id, b"i2");
        let (i2a_id, i2a_tx) = ins(ch, i1_id, b"i2'");
        let (i2_hash, i2a_hash) = (i2_tx.hash(), i2a_tx.hash());
        let ba = api_block(1, 0, 1, vec![i1_tx]);
        let bb = api_block(2, 1, 2, vec![i2_tx]);
        let bba = api_block(3, 1, 3, vec![i2a_tx]);
        let bca = api_block(4, 3, 4, Vec::new());
        let bc = api_block(5, 2, 5, Vec::new());
        let bd = api_block(6, 5, 6, Vec::new());
        let bx = api_block(7, 1, 7, Vec::new());
        let by = api_block(8, 7, 8, Vec::new());
        let bz = api_block(9, 8, 9, Vec::new());

        let mut state = None;
        let r = drive(
            &mut state,
            ch,
            &[
                live_event(&ba),
                live_event(&bb),
                fork_event(&bba, &bb),
                live_event(&bca),
                fork_event(&bc, &bca),
                live_event(&bd),
                fork_event(&bx, &bd),
                fork_event(&by, &bd),
                live_event(&bz),
            ],
        )
        .await;

        let u = r[3].result.channel_update.as_ref().expect("B' wins");
        assert_eq!(msg_ids(&u.orphaned), vec![i2_id]);
        assert_eq!(msg_ids(&u.adopted), vec![i2a_id]);
        let u = r[5].result.channel_update.as_ref().expect("B wins back");
        assert_eq!(msg_ids(&u.orphaned), vec![i2a_id]);
        assert_eq!(msg_ids(&u.adopted), vec![i2_id]);
        assert!(
            r[8].result.channel_update.is_none(),
            "bare un-mine of B: i2 is pending again"
        );
        // `common_prefix ++ adopted` is the view above LIB on every event.
        assert!(r[0].result.common_prefix.is_empty());
        assert_eq!(msg_ids(&r[1].result.common_prefix), vec![i1_id]);
        assert_eq!(msg_ids(&r[3].result.common_prefix), vec![i1_id]);
        assert_eq!(msg_ids(&r[8].result.common_prefix), vec![i1_id, i2_id]);
        // Each switch sheds only the loser; the winner, re-mirrored from the
        // store, reads as mined on its branch.
        let shed: Vec<TxHash> = r[3].shed.iter().map(PendingTx::tx_hash).collect();
        assert_eq!(shed, vec![i2_hash]);
        let shed: Vec<TxHash> = r[5].shed.iter().map(PendingTx::tx_hash).collect();
        assert_eq!(shed, vec![i2a_hash]);
        assert!(r[8].shed.is_empty());
        let s = state.as_ref().unwrap();
        let resubmit: Vec<TxHash> = s
            .pending_txs(bz.header.id)
            .iter()
            .map(|(hash, _)| *hash)
            .collect();
        assert_eq!(
            resubmit,
            vec![i2_hash],
            "i2 is unmined at Z and goes back out"
        );
    }

    /// An L1 branch change served by the canonical backfill: the new branch
    /// re-mines the old branch's inscription A and adds Y on top. Only Y is
    /// news to the consumer — A must be neither re-adopted (it was already
    /// reported) nor orphaned (its position is intact on the new branch).
    #[tokio::test]
    async fn reorg_backfill_reports_only_new_inscriptions_as_adopted() {
        // Chain: G(0) <- B1            (old branch, delivered live)
        //        G(0) <- C1 <- C2 <- C3 (new branch, C1/C2 via backfill)
        //   B1 and C1 both carry inscription A (same tx, re-mined)
        //   C2 carries inscription Y (parent = A)
        //   C3 is empty, delivered live with C2/C1 missing
        let channel_id = ChannelId::from([0u8; 32]);

        let a = inscribe_op(channel_id, MsgId::root(), b"a");
        let a_id = a.id();
        let y = inscribe_op(channel_id, a_id, b"y");
        let y_id = y.id();
        let a_tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(a)]);
        let y_tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(y)]);

        let b1 = api_block(1, 0, 1, vec![a_tx.clone()]);
        let c1 = api_block(4, 0, 2, vec![a_tx]);
        let c2 = api_block(5, 4, 3, vec![y_tx]);
        let c3 = api_block(6, 5, 4, Vec::new());

        let node = MockNode {
            blocks: vec![c1, c2],
            ..MockNode::default()
        };
        let mut state = None;
        let mut current_tip = None;
        let mut lib_slot = Slot::genesis();

        let first = handle_block_event(
            &live_event(&b1),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B1 succeeds");
        assert!(
            first.channel_update.is_some(),
            "sanity: A is adopted on the first event"
        );

        let second = handle_block_event(
            &live_event(&c3),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing C3 succeeds");
        let update = second
            .channel_update
            .expect("the backfilled branch added Y; expected a channel update");
        assert!(
            update
                .adopted
                .iter()
                .any(|t| t.inscription().is_some_and(|i| i.this_msg == y_id)),
            "inscription mined on the backfilled branch must be reported as adopted; \
             got adopted={:?}, orphaned={:?}",
            update.adopted,
            update.orphaned,
        );
        assert!(
            !update
                .adopted
                .iter()
                .any(|t| t.inscription().is_some_and(|i| i.this_msg == a_id)),
            "re-mined A was already reported adopted and must not echo"
        );
        assert!(
            update.orphaned.is_empty(),
            "A's position is intact on the new branch; nothing is orphaned, got {:?}",
            update.orphaned,
        );
        assert_eq!(update.new_channel_tip, y_id);
    }

    /// Blocks pulled in by the LIB backfill surface exclusively through
    /// `finalized`: not double-reported as adopted, not misreported as
    /// orphaned.
    #[tokio::test]
    async fn lib_backfilled_blocks_surface_as_finalized_not_adopted() {
        // Chain: G(0) <- B1 <- B2 <- B3, LIB advances to B2 on the second
        // event.
        //   B1 carries inscription A (parent = root), delivered live
        //   B2 carries inscription Y (parent = A), never seen live; reaches
        //      state only through the LIB backfill of slots 1..=2
        //   B3 is empty, delivered live with LIB at B2
        let channel_id = ChannelId::from([0u8; 32]);

        let a = inscribe_op(channel_id, MsgId::root(), b"a");
        let a_id = a.id();
        let y = inscribe_op(channel_id, a_id, b"y");
        let y_id = y.id();

        let b1 = api_block(
            1,
            0,
            1,
            vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(a)])],
        );
        let b2 = api_block(
            2,
            1,
            2,
            vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(y)])],
        );
        let b3 = api_block(3, 2, 3, Vec::new());

        let node = MockNode {
            immutable: vec![b1.clone(), b2],
            ..MockNode::default()
        };
        let mut state = None;
        let mut current_tip = None;
        let mut lib_slot = Slot::genesis();

        let first = handle_block_event(
            &live_event(&b1),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B1 succeeds");
        assert!(
            first.channel_update.is_some(),
            "sanity: A is adopted on the first event"
        );

        let event = ProcessedBlockEvent {
            block: b3,
            tip: header_id(3),
            tip_slot: Slot::from(3),
            lib: header_id(2),
            lib_slot: Slot::from(2),
        };
        let second = handle_block_event(
            &event,
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B3 succeeds");

        let finalized_msgs: Vec<MsgId> = second
            .finalized_items
            .iter()
            .flat_map(|t| t.ops.iter())
            .filter_map(|op| match op {
                FinalizedOp::Inscription(i) => Some(i.this_msg),
                _ => None,
            })
            .collect();
        assert!(
            finalized_msgs.contains(&a_id) && finalized_msgs.contains(&y_id),
            "both inscriptions finalize via the LIB backfill; got {finalized_msgs:?}"
        );

        if let Some(update) = second.channel_update {
            assert!(
                !update
                    .adopted
                    .iter()
                    .any(|t| t.inscription().is_some_and(|i| i.this_msg == y_id)),
                "LIB-backfilled Y reaches the consumer as finalized and must not \
                 double-report as adopted"
            );
            assert!(
                update.orphaned.is_empty(),
                "content finalized by this event must not be misreported as orphaned; \
                 got {:?}",
                update.orphaned,
            );
        }
    }

    /// A finalized inscription must never be reported `orphaned`: LIB
    /// advancing past its block (steady state, no reorg) removes it from
    /// the lineage walk's range but not from the channel.
    #[tokio::test]
    async fn finalized_inscription_is_not_reported_orphaned() {
        // Chain: G(0) <- B1(A) <- B2 <- B3, all delivered live; LIB advances
        // one block per event: genesis, B1, B2.
        let channel_id = ChannelId::from([0u8; 32]);
        let a = inscribe_op(channel_id, MsgId::root(), b"a");
        let b1 = api_block(
            1,
            0,
            1,
            vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(a)])],
        );
        let b2 = api_block(2, 1, 2, Vec::new());
        let b3 = api_block(3, 2, 3, Vec::new());

        let node = MockNode {
            immutable: vec![b1.clone(), b2.clone()],
            ..MockNode::default()
        };
        let mut state = None;
        let mut current_tip = None;
        let mut lib_slot = Slot::genesis();

        handle_block_event(
            &live_event(&b1),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B1 succeeds");

        // LIB advances to B1 — the block carrying A. A finalizes here.
        let e2 = ProcessedBlockEvent {
            block: b2,
            tip: header_id(2),
            tip_slot: Slot::from(2),
            lib: header_id(1),
            lib_slot: Slot::from(1),
        };
        let second = handle_block_event(
            &e2,
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B2 succeeds");
        assert!(
            second
                .channel_update
                .as_ref()
                .is_none_or(|u| u.orphaned.is_empty()),
            "nothing is orphaned when A's block becomes LIB; got {:?}",
            second.channel_update,
        );
        assert_eq!(
            state.as_ref().unwrap().pending_publish_count(),
            0,
            "A finalized: it leaves pending and must not be re-mirrored from its (LIB) block"
        );

        // LIB advances to B2 — A's block is now strictly below LIB and its
        // store entry is pruned.
        let e3 = ProcessedBlockEvent {
            block: b3,
            tip: header_id(3),
            tip_slot: Slot::from(3),
            lib: header_id(2),
            lib_slot: Slot::from(2),
        };
        let third = handle_block_event(
            &e3,
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B3 succeeds");
        assert!(
            third
                .channel_update
                .as_ref()
                .is_none_or(|u| u.orphaned.is_empty()),
            "a finalized inscription must never be reported orphaned; got {:?}",
            third.channel_update,
        );
        let s = state.as_mut().unwrap();
        assert_eq!(s.pending_publish_count(), 0);
        assert!(
            s.shed_off_branch_pending(header_id(3)).is_empty(),
            "nothing pending to shed: a resurrected finalized entry would be reported orphaned here"
        );
    }

    /// End-to-end wallet tracking through the live + finalized paths: a live
    /// deposit surfaces as an unfinalized note, a live transfer re-keys it,
    /// and the LIB backfill folds the result into the finalized base without
    /// double-counting.
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "End-to-end wallet tracking scenario."
    )]
    async fn wallet_tracks_deposit_and_transfer_through_finalization() {
        let channel_id = ChannelId::from([0u8; 32]);
        let recreated = NoteId::from(Fr::from(1010u64));
        let out_pk = lb_key_management_system_service::keys::ZkPublicKey::from(Fr::from(7u64));

        let dep = deposit_op(channel_id, 1, Metadata::try_from(b"d".to_vec()).unwrap());
        let dep_tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(dep.clone())]);
        let b1 = api_block(1, 0, 1, vec![dep_tx.clone()]);

        let transfer = ChannelTransferOp {
            channel_id,
            inputs: Inputs::new([recreated]),
            outputs: Outputs::new([Note::new(50, out_pk)]),
        };
        let out_id = transfer.utxos().next().unwrap().id();
        let transfer_tx = unverified_tx_with_ops(vec![Op::ChannelTransfer(transfer)]);
        let b2 = api_block(2, 1, 2, vec![transfer_tx]);
        let b3 = api_block(3, 2, 3, Vec::new());

        let recreated_note = DepositNote {
            note_id: recreated,
            value: 50,
            pk: out_pk,
        };
        let node = MockNode {
            immutable: vec![b1.clone(), b2.clone()],
            events: HashMap::from([(
                header_id(1),
                deposit_event(&dep_tx, &dep, 50, vec![recreated_note]),
            )]),
            ..MockNode::default()
        };
        let mut state = None;
        let mut current_tip = None;
        let mut lib_slot = Slot::genesis();

        handle_block_event(
            &live_event(&b1),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B1 succeeds");
        let view = state
            .as_ref()
            .unwrap()
            .channel_wallet_view(Some(header_id(1)));
        assert!(view.finalized.is_empty());
        assert_eq!(view.unfinalized.len(), 1);
        assert_eq!(view.unfinalized[0].note_id, recreated);
        assert_eq!(view.unfinalized[0].value, 50);

        handle_block_event(
            &live_event(&b2),
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B2 succeeds");
        let view = state
            .as_ref()
            .unwrap()
            .channel_wallet_view(Some(header_id(2)));
        assert_eq!(view.unfinalized.len(), 1, "transfer re-keys the note");
        assert_eq!(view.unfinalized[0].note_id, out_id);
        assert_eq!(view.unfinalized[0].pk, out_pk);

        // LIB advances to B2: the immutable range folds into the base.
        let event = ProcessedBlockEvent {
            block: b3,
            tip: header_id(3),
            tip_slot: Slot::from(3),
            lib: header_id(2),
            lib_slot: Slot::from(2),
        };
        let result = handle_block_event(
            &event,
            &mut state,
            &mut current_tip,
            &mut lib_slot,
            channel_id,
            &node,
        )
        .await
        .expect("processing B3 succeeds");

        let view = state
            .as_ref()
            .unwrap()
            .channel_wallet_view(Some(header_id(3)));
        assert_eq!(view.finalized.len(), 1);
        assert_eq!(view.finalized[0].note_id, out_id);
        assert!(
            view.unfinalized.is_empty(),
            "folded overlay entries must not double-count"
        );

        // The finalized stream surfaces the transfer op to consumers.
        assert!(
            result
                .finalized_items
                .iter()
                .flat_map(|t| t.ops.iter())
                .any(|op| matches!(op, FinalizedOp::ChannelTransfer(t) if t.op.channel_id == channel_id)),
            "finalized items carry the channel transfer"
        );
    }
}
