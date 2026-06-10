use std::collections::HashMap;

use lb_common_http_client::{ProcessedBlockEvent, Slot};
use lb_core::{
    crypto::Hash,
    header::HeaderId,
    mantle::{
        SignedMantleTx, Transaction as _, Value,
        ops::{
            Op, OpId as _,
            channel::{ChannelId, MsgId, inscribe::Inscription},
        },
        tx::TxHash,
    },
};
use tracing::{debug, error, warn};

use super::{
    TARGET,
    state::{ChannelUpdateInfo, TxState},
    types::{
        DepositInfo, Error, FinalizedOp, FinalizedTx, InscriptionInfo, OrphanedTx, PendingTx,
        WithdrawInfo,
    },
};
use crate::{adapter, adapter::build_deposit_amounts};

/// Result of processing a block event.
pub(super) struct BlockEventResult {
    /// Finalized channel txs in tx/op execution order across blocks. Each
    /// [`FinalizedTx`] groups all channel-relevant ops from a single Mantle
    /// tx — inscriptions (ours or others'), deposits (with `amount` from the
    /// chain events API) and withdraws (standalone or part of an atomic
    /// inscription+withdraw bundle).
    pub(super) finalized_items: Vec<FinalizedTx>,
    pub(super) channel_update: Option<ChannelUpdateInfo>,
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
    let block_id = event.block.header.id;
    let parent_id = event.block.header.parent_block;
    let tip = event.tip;
    let lib = event.lib;

    // Initialize state on first event
    if state.is_none() {
        *state = Some(TxState::new(lib, MsgId::root()));
    }

    let Some(s) = state.as_mut() else {
        return Ok(BlockEventResult {
            finalized_items: Vec::new(),
            channel_update: None,
        });
    };

    let old_tip = *current_tip;

    // Backfill if needed (self-healing on every event)
    // 1. Backfill finalized blocks up to LIB (only when state's LIB is behind).
    //    Done BEFORE we advance `*lib_slot` and BEFORE we mutate state for the live
    //    event — so on a fetch failure the caller can retry the same event next
    //    time around.
    let mut lib_finalized = Vec::new();
    let mut finalized_items: Vec<FinalizedTx> = Vec::new();
    if lib != s.lib() {
        let new_lib_slot = event.lib_slot;
        let from: u64 = (*lib_slot).into();
        let to: u64 = new_lib_slot.into();
        if from < to {
            let batch = fetch_and_process_blocks(s, from + 1, to, channel_id, node).await?;
            lib_finalized = batch.our_tx_hashes;
            finalized_items = batch.items;
        }
        *lib_slot = new_lib_slot;
    }

    // 2. Backfill canonical chain if parent is missing
    if !s.has_block(&parent_id) && parent_id != s.lib() {
        backfill_canonical(s, parent_id, channel_id, node).await;
    }

    // Extract tx hashes and inscription info for our channel
    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| touches_channel_tip(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    let inscriptions = extract_channel_tip_ops(&event.block.transactions, channel_id);

    // Process the actual event block
    s.process_block(block_id, parent_id, lib, our_txs, inscriptions);

    // Remove our pending txs that were finalized in the backfilled LIB blocks.
    // `finalized_items` already carries the typed payloads (built before
    // pending was mutated) so we just need to clean up state here.
    for tx_hash in &lib_finalized {
        s.remove_pending(tx_hash);
    }

    *current_tip = Some(tip);

    // Detect channel changes.
    // On first event (old_tip is None), check for existing inscriptions on
    // the channel — this handles clean start on an existing channel.
    // On subsequent events, detect channel update if tip changed.
    let channel_update = match old_tip {
        Some(old) if old != tip => s.detect_channel_update(old, tip),
        None => {
            // First event — no old canonical exists yet, so nothing can be
            // orphaned. Report any inscriptions on the initial tip as adopted.
            let channel_tip = s.channel_tip_at(tip);
            if channel_tip == MsgId::root() {
                None
            } else {
                let adopted = s.collect_inscriptions_on_branch(tip);
                (!adopted.is_empty()).then_some(ChannelUpdateInfo {
                    orphaned: Vec::new(),
                    adopted,
                    new_channel_tip: channel_tip,
                })
            }
        }
        _ => None, // tip unchanged
    };

    Ok(BlockEventResult {
        finalized_items,
        channel_update,
    })
}

/// Convert a shed pending entry into an [`OrphanedTx`] for surfacing to the
/// consumer.
pub(super) fn orphan_from_shed(entry: PendingTx) -> OrphanedTx {
    let info = entry.inscription();
    debug!(
        target: TARGET,
        "  orphaned: payload={:?}, tx={}, msg_id={}",
        String::from_utf8_lossy(&info.payload),
        hex::encode(info.tx_hash.0),
        hex::encode(info.this_msg.as_ref()),
    );
    match entry {
        PendingTx::Inscription(i) => OrphanedTx::Inscription(i),
        PendingTx::AtomicWithdraw(a) => OrphanedTx::AtomicWithdraw(a),
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

/// Fetch blocks in a slot range, process them into state, and return our
/// finalized tx hashes plus the user-facing items grouped per Mantle tx.
///
/// State is mutated only after the per-block fetch (blocks + events) has
/// fully succeeded. On any failure the function returns [`Err`] without
/// having advanced `state` for the failing block (earlier blocks in the
/// range are kept — they were independent successful units of work). The
/// caller is expected to abandon the current attempt and retry the range
/// later; the partial advance ensures progress on transient errors that
/// resolve mid-range.
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
    let mut result = FetchedBatch {
        our_tx_hashes: Vec::new(),
        items: Vec::new(),
    };

    let blocks = node
        .immutable_blocks(Slot::from(from_slot), Slot::from(to_slot))
        .await
        .map_err(|e| {
            error!(target: TARGET, ?from_slot, ?to_slot, ?e, "Failed to fetch immutable blocks");
            Error::Network(format!(
                "failed to fetch blocks (slots {from_slot}..{to_slot}): {e}"
            ))
        })?;

    for block in blocks {
        let our_txs: Vec<TxHash> = block
            .transactions
            .iter()
            .filter(|tx| touches_channel_tip(tx, channel_id))
            .map(|tx| tx.mantle_tx.hash())
            .collect();

        let inscriptions = extract_channel_tip_ops(&block.transactions, channel_id);

        // Fetch + validate deposit events for this block BEFORE mutating
        // state — on error we leave state untouched so the caller can retry.
        let deposit_amounts =
            fetch_block_deposit_amounts(node, block.header.id, &block.transactions, channel_id)
                .await?;
        let block_items =
            extract_finalized_items(&block.transactions, channel_id, &deposit_amounts);

        result.our_tx_hashes.extend(our_txs.iter().copied());
        result.items.extend(block_items);

        let current_lib = state.lib();
        state.process_block(
            block.header.id,
            block.header.parent_block,
            current_lib,
            our_txs,
            inscriptions,
        );
    }

    Ok(result)
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
async fn fetch_block_deposit_amounts<Node>(
    node: &Node,
    block_id: HeaderId,
    transactions: &[SignedMantleTx],
    channel_id: ChannelId,
) -> Result<HashMap<(TxHash, Hash), Value>, Error>
where
    Node: adapter::Node + Sync,
{
    let expected: Vec<(TxHash, Hash)> = transactions
        .iter()
        .flat_map(|tx| {
            let tx_hash = tx.mantle_tx.hash();
            tx.mantle_tx.ops().iter().filter_map(move |op| match op {
                Op::ChannelDeposit(d) if d.channel_id == channel_id => Some((tx_hash, d.op_id())),
                _ => None,
            })
        })
        .collect();

    if expected.is_empty() {
        return Ok(HashMap::new());
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

    let amounts = build_deposit_amounts(&events);
    for key in &expected {
        if !amounts.contains_key(key) {
            error!(
                target: TARGET,
                ?block_id,
                tx_hash = ?key.0,
                op_id = ?key.1,
                "Block events missing an entry for a known channel deposit op; \
                 expected atomic block/events visibility per node semantics"
            );
            return Err(Error::Network(format!(
                "block {block_id} events missing deposit entry for tx {:?} op {:?}",
                key.0, key.1
            )));
        }
    }
    Ok(amounts)
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
/// off the running tip) is asserted by [`extract_channel_tip_ops`], which
/// every caller runs on the same `transactions` before this walker.
///
/// Deposits without a matching event entry are skipped with a warning.
fn extract_finalized_items(
    transactions: &[SignedMantleTx],
    channel_id: ChannelId,
    deposit_amounts: &HashMap<(TxHash, Hash), Value>,
) -> Vec<FinalizedTx> {
    let mut items: Vec<FinalizedTx> = Vec::new();
    let mut last_in_block: Option<MsgId> = None;

    for tx in transactions {
        let tx_hash = tx.mantle_tx.hash();
        let mut ops: Vec<FinalizedOp> = Vec::new();
        for op in tx.mantle_tx.ops() {
            match op {
                Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                    // Chain order is asserted by `extract_channel_tip_ops`,
                    // which runs on the same `transactions` before this
                    // walker on every call site (live + backfill).
                    let info = InscriptionInfo {
                        tx_hash,
                        parent_msg: inscribe.parent,
                        this_msg: inscribe.id(),
                        payload: inscribe.inscription.clone(),
                    };
                    last_in_block = Some(info.this_msg);
                    ops.push(FinalizedOp::Inscription(info));
                }
                Op::ChannelConfig(config) if config.channel == channel_id => {
                    // `ChannelConfig` has no parent on the wire — it
                    // unconditionally overwrites the channel tip. Synthetic
                    // entry below keeps `channel_tip` in sync; we set
                    // parent_msg to the prior in-block tip so the value
                    // remains coherent if a consumer inspects it.
                    let parent_msg = last_in_block.unwrap_or_else(MsgId::root);
                    let info = InscriptionInfo {
                        tx_hash,
                        parent_msg,
                        this_msg: config.id(),
                        payload: Inscription::new_unchecked(Vec::new()),
                    };
                    last_in_block = Some(info.this_msg);
                    ops.push(FinalizedOp::Inscription(info));
                }
                Op::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
                    let op_id = deposit.op_id();
                    // `fetch_block_deposit_amounts` validates that every
                    // channel-deposit op in the block has a matching event
                    // entry before returning, so the lookup is infallible
                    // here. A miss would be a caller-side bug.
                    let &amount = deposit_amounts.get(&(tx_hash, op_id)).expect(
                        "deposit_amounts must contain every channel deposit op - \
                         fetch_block_deposit_amounts invariant",
                    );
                    ops.push(FinalizedOp::Deposit(DepositInfo {
                        tx_hash,
                        op_id,
                        channel_id,
                        inputs: deposit.inputs.clone(),
                        amount,
                        metadata: deposit.metadata.clone(),
                    }));
                }
                Op::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
                    ops.push(FinalizedOp::Withdraw(WithdrawInfo {
                        tx_hash,
                        op: withdraw.clone(),
                    }));
                }
                _ => {}
            }
        }
        if !ops.is_empty() {
            items.push(FinalizedTx { tx_hash, ops });
        }
    }

    items
}

/// Backfill canonical chain backwards from a missing parent to LIB.
///
/// Uses `state.lib()` during replay to avoid premature finalization.
/// The caller is responsible for triggering finalization after backfill
/// completes.
async fn backfill_canonical<Node>(
    state: &mut TxState,
    missing_parent: HeaderId,
    channel_id: ChannelId,
    node: &Node,
) where
    Node: adapter::Node + Sync,
{
    debug!(target: TARGET, "Backfilling canonical chain from {:?}", missing_parent);
    let blocks = walk_back_to_known(state, missing_parent, node).await;
    let lib = state.lib();
    for block in &blocks {
        apply_backfilled_block(state, block, channel_id, lib);
    }
    debug!(target: TARGET, "Canonical backfill complete");
}

/// Walk backwards from `from` until a block the state already knows about (or
/// LIB) is reached. Returns blocks in forward order (oldest first).
async fn walk_back_to_known<Node>(
    state: &TxState,
    from: HeaderId,
    node: &Node,
) -> Vec<lb_common_http_client::ApiBlock>
where
    Node: adapter::Node + Sync,
{
    let mut blocks = Vec::new();
    let mut current = from;
    let lib = state.lib();

    while !state.has_block(&current) && current != lib {
        match node.block(current).await {
            Ok(Some(block)) => {
                let parent = block.header.parent_block;
                blocks.push(block);
                current = parent;
            }
            Ok(None) => {
                warn!(target: TARGET, "Block {:?} not found during canonical backfill", current);
                break;
            }
            Err(e) => {
                warn!(
                    target: TARGET,
                    "Failed to fetch block {:?} during canonical backfill: {e}",
                    current
                );
                break;
            }
        }
    }

    blocks.reverse();
    blocks
}

fn apply_backfilled_block(
    state: &mut TxState,
    block: &lb_common_http_client::ApiBlock,
    channel_id: ChannelId,
    lib: HeaderId,
) {
    let block_id = block.header.id;
    let parent_id = block.header.parent_block;

    let our_txs: Vec<TxHash> = block
        .transactions
        .iter()
        .filter(|tx| touches_channel_tip(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    let inscriptions = extract_channel_tip_ops(&block.transactions, channel_id);

    // Use current state lib to avoid premature finalization
    state.process_block(block_id, parent_id, lib, our_txs, inscriptions);
}

/// Extract every op in a block that advances the channel's tip pointer, in
/// tx-then-op order. Returns one [`InscriptionInfo`] per tip-advancing op —
/// both real inscriptions (`ChannelInscribe`) and synthetic entries for
/// `ChannelConfig` (which resets the tip per spec). Synthetic config
/// entries carry an empty payload so payload-keyed consumers ignore them
/// naturally.
///
/// The ledger validates ops in tx-then-op order, with each `ChannelInscribe`
/// requiring `parent == channel.tip_message` and each `ChannelConfig`
/// unconditionally overwriting the tip. A block in which tip-advancing ops
/// for one channel appear out of chain order would fail validation, so
/// tx-then-op order is already chain order — callers (e.g. `channel_tip_at`)
/// can rely on `last()` being the post-block tip. We verify this trust
/// assumption with an inline assertion: each `ChannelInscribe`'s `parent`
/// must equal the running in-block tip. A mismatch panics rather than
/// silently re-deriving order, because the same node bug could produce an
/// undetectable mis-ordering elsewhere.
///
/// Same-block `ChannelConfig` replay (e.g. `[Inscribe, Config X, Inscribe,
/// Config X]`) yields items with a repeated `this_msg = hash(X)`. That's
/// the correct representation: at the ledger, the final tip is `hash(X)`,
/// matching `items.last().this_msg`. The running tip stays at `hash(X)`
/// after a repeat, so the inscription-chain assertion continues to verify
/// subsequent entries. Downstream consumers that want unique-by-`this_msg`
/// semantics dedup themselves.
fn extract_channel_tip_ops(txs: &[SignedMantleTx], channel_id: ChannelId) -> Vec<InscriptionInfo> {
    let mut items: Vec<InscriptionInfo> = Vec::new();
    let hash_and_ops = txs
        .iter()
        .flat_map(|tx| std::iter::repeat(tx.mantle_tx.hash()).zip(tx.mantle_tx.ops().iter()));

    for (tx_hash, op) in hash_and_ops {
        match op {
            Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                if let Some(prev) = items.last() {
                    assert_eq!(
                        inscribe.parent, prev.this_msg,
                        "block delivered inscription out of execution order: \
                         inscribe.parent {:?} does not chain off the prior in-block tip {:?}",
                        inscribe.parent, prev.this_msg
                    );
                }
                items.push(InscriptionInfo {
                    tx_hash,
                    parent_msg: inscribe.parent,
                    this_msg: inscribe.id(),
                    payload: inscribe.inscription.clone(),
                });
            }
            Op::ChannelConfig(config) if config.channel == channel_id => {
                // `ChannelConfig` has no parent on the wire — it unconditionally
                // overwrites the channel tip. The `parent_msg` field below is
                // unused by lineage walks for config entries; we set it to the
                // prior in-block tip (or root) so the value remains coherent
                // with surrounding entries if a consumer ever inspects it.
                let parent_msg = items.last().map_or_else(MsgId::root, |prev| prev.this_msg);
                items.push(InscriptionInfo {
                    tx_hash,
                    parent_msg,
                    this_msg: config.id(),
                    payload: [].into(),
                });
            }
            _ => {}
        }
    }

    items
}

/// True iff this tx contains any op that advances our channel's tip pointer
/// (`ChannelInscribe` or `ChannelConfig`). Deposits and withdraws don't move
/// the tip and so don't make a tx "ours" for tip-tracking purposes.
fn touches_channel_tip(tx: &SignedMantleTx, channel_id: ChannelId) -> bool {
    tx.mantle_tx.ops().iter().any(|op| match op {
        Op::ChannelInscribe(inscribe) => inscribe.channel_id == channel_id,
        Op::ChannelConfig(set_keys) => set_keys.channel == channel_id,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{
        MantleTx, Note,
        channel::{SlotTimeframe, SlotTimeout},
        encoding::Ops,
        ledger::{Inputs, Outputs},
        ops::{
            OpId as _, OpProof,
            channel::{
                config::{ChannelConfigOp, Keys},
                deposit::{DepositOp, Metadata},
                inscribe::InscriptionOp,
                withdraw::ChannelWithdrawOp,
            },
        },
    };
    use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature, ZkKey};
    use num_bigint::BigUint;

    use super::*;

    /// Build a `SignedMantleTx` carrying the given ops, with placeholder
    /// proofs. Suitable for tests that only care about op extraction, not
    /// verification.
    fn unverified_tx_with_ops(ops: Vec<Op>) -> SignedMantleTx {
        let n = ops.len();
        let mantle_tx = MantleTx(Ops::try_from(ops).unwrap());
        SignedMantleTx::new_unverified(
            mantle_tx,
            vec![OpProof::Ed25519Sig(Ed25519Signature::zero()); n],
        )
    }

    fn deposit_op(channel_id: ChannelId, input_seed: u32, metadata: Metadata) -> DepositOp {
        use lb_core::mantle::NoteId;
        use lb_groth16::Fr;
        DepositOp {
            channel_id,
            inputs: Inputs::new([NoteId::from(Fr::from(input_seed))]),
            metadata,
        }
    }

    /// Extract deposits via the unified walker and filter to deposit entries
    /// for assertion clarity.
    fn extract_deposits_for_test(
        transactions: &[SignedMantleTx],
        channel_id: ChannelId,
        amounts: &HashMap<(TxHash, Hash), u64>,
    ) -> Vec<DepositInfo> {
        extract_finalized_items(transactions, channel_id, amounts)
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
        let tx_hash = tx.mantle_tx.hash();

        let mut amounts = HashMap::new();
        amounts.insert((tx_hash, our_op_id), 1234u64);

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
    #[should_panic(expected = "fetch_block_deposit_amounts invariant")]
    fn extract_finalized_items_panics_if_deposit_amounts_incomplete() {
        // The walker contract: `deposit_amounts` must contain an entry for
        // every channel-deposit op in the input transactions. This is
        // enforced upstream by `fetch_block_deposit_amounts`, which validates
        // completeness and errors out before the walker is ever called with a
        // gap. A panic here surfaces the bug immediately if a future caller
        // violates that invariant — silent skip would drop a real deposit.
        let channel_id = ChannelId::from([0; 32]);
        let op = deposit_op(channel_id, 1, b"to Alice".into());
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(op)]);
        drop(extract_finalized_items(
            std::slice::from_ref(&tx),
            channel_id,
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
        let hash_a = tx_a.mantle_tx.hash();
        let hash_b = tx_b.mantle_tx.hash();

        let mut amounts = HashMap::new();
        amounts.insert((hash_a, id1), 10);
        amounts.insert((hash_a, id2), 20);
        amounts.insert((hash_b, id3), 30);

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
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };

        let tx =
            unverified_tx_with_ops(vec![Op::ChannelDeposit(dep), Op::ChannelInscribe(inscribe)]);
        let tx_hash = tx.mantle_tx.hash();

        let mut amounts = HashMap::new();
        amounts.insert((tx_hash, dep_op_id), 500u64);

        let items = extract_finalized_items(std::slice::from_ref(&tx), channel_id, &amounts);

        assert_eq!(items.len(), 1, "one FinalizedTx for the single Mantle tx");
        assert_eq!(items[0].tx_hash, tx_hash);
        assert_eq!(items[0].ops.len(), 2);
        assert!(matches!(items[0].ops[0], FinalizedOp::Deposit(_)));
        assert!(matches!(items[0].ops[1], FinalizedOp::Inscription(_)));
    }

    #[test]
    fn extract_finalized_items_surfaces_standalone_withdraw() {
        // A ChannelWithdraw not bundled with an inscription (e.g. from
        // another sequencer or future multi-sig) should still surface as
        // a FinalizedOp::Withdraw — the sequencer stream is the complete
        // finalized view, not a "what we tracked locally" view.
        let channel_id = ChannelId::from([0; 32]);
        let other_channel = ChannelId::from([9; 32]);
        let outputs = Outputs::new([Note::new(
            42,
            ZkKey::from(BigUint::from(0u64)).to_public_key(),
        )]);
        let withdraw_for_us = ChannelWithdrawOp {
            channel_id,
            outputs: outputs.clone(),
            withdraw_nonce: 7,
        };
        let withdraw_other = ChannelWithdrawOp {
            channel_id: other_channel,
            outputs,
            withdraw_nonce: 0,
        };

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelWithdraw(withdraw_for_us),
            Op::ChannelWithdraw(withdraw_other),
        ]);
        let tx_hash = tx.mantle_tx.hash();

        let items = extract_finalized_items(std::slice::from_ref(&tx), channel_id, &HashMap::new());

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tx_hash, tx_hash);
        assert_eq!(items[0].ops.len(), 1, "only our channel's withdraw");
        match &items[0].ops[0] {
            FinalizedOp::Withdraw(w) => {
                assert_eq!(w.tx_hash, tx_hash);
                assert_eq!(w.op.channel_id, channel_id);
                assert_eq!(w.op.withdraw_nonce, 7);
            }
            other => panic!("expected Withdraw, got {other:?}"),
        }
    }

    fn channel_config(channel_id: ChannelId) -> ChannelConfigOp {
        let signer = Ed25519Key::from_bytes(&[0u8; 32]).public_key();
        ChannelConfigOp {
            channel: channel_id,
            keys: Keys::try_from(vec![signer]).unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            withdraw_threshold: 1,
        }
    }

    fn inscribe_op(channel_id: ChannelId, parent: MsgId, payload: &[u8]) -> InscriptionOp {
        InscriptionOp {
            channel_id,
            inscription: Inscription::new_unchecked(payload.to_vec()),
            parent,
            signer: Ed25519Key::from_bytes(&[0u8; 32]).public_key(),
        }
    }

    fn header_id(n: u8) -> HeaderId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        HeaderId::from(bytes)
    }

    fn dummy_pending_tx(seed: u8) -> SignedMantleTx {
        let mantle_tx = MantleTx(
            [Op::ChannelInscribe(InscriptionOp {
                channel_id: [0u8; 32].into(),
                inscription: Inscription::new_unchecked(vec![seed]),
                parent: MsgId::root(),
                signer: Ed25519Key::from_bytes(&[seed; 32]).public_key(),
            })]
            .into(),
        );
        SignedMantleTx::new_unverified(
            mantle_tx,
            vec![OpProof::Ed25519Sig(Ed25519Signature::zero())],
        )
    }

    /// Run a synchronous callable on a background thread and bail out if it
    /// doesn't return within `timeout`. Used so the toposort cycle hazard
    /// surfaces as a clear test failure rather than hanging CI.
    fn run_with_timeout<R: Send + 'static>(
        timeout: std::time::Duration,
        f: impl FnOnce() -> R + Send + 'static,
    ) -> R {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(tx.send(f()));
        });
        rx.recv_timeout(timeout)
            .expect("extraction hung (suspected toposort cycle in extract_channel_tip_ops)")
    }

    #[test]
    fn same_block_config_replay_interleaved_yields_correct_tip_and_pending_orphans() {
        // Flow under test:
        //   1. extract_channel_tip_ops on a block with same-block config replay
        //   2. feed items into TxState::process_block
        //   3. channel_tip_at must equal the replayed config id (ledger truth)
        //   4. pending lineage anchored to a now-stale ancestor must be shed
        //      off-branch; pending anchored to the new tip must stay.
        //
        // Block layout: [Inscribe I1 (parent=root), ChannelConfig X,
        //                Inscribe I2 (parent=X), ChannelConfig X].
        // At the ledger this validates left-to-right and the final
        // channel.tip_message is hash(X) — the replay reset it.
        let channel_id = ChannelId::from([0u8; 32]);
        let config = channel_config(channel_id);
        let config_id = config.id();
        let i1 = inscribe_op(channel_id, MsgId::root(), b"i1");
        let i1_id = i1.id();
        let i2 = inscribe_op(channel_id, config_id, b"i2");

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(i1),
            Op::ChannelConfig(config.clone()),
            Op::ChannelInscribe(i2),
            Op::ChannelConfig(config),
        ]);
        let tx_hash = tx.mantle_tx.hash();

        // Stage pending inscriptions BEFORE driving the block through.
        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        // Pending chained from I1 — invalidated by the replay (tip moved off I1).
        let pending_stale = dummy_pending_tx(1);
        let pending_stale_hash = pending_stale.mantle_tx.hash();
        state.submit_inscription(
            pending_stale,
            i1_id,
            MsgId::from([99u8; 32]),
            Inscription::new_unchecked(b"chained-from-i1".to_vec()),
        );

        // Pending chained from the config tip — should remain on-branch.
        let pending_live = dummy_pending_tx(2);
        let pending_live_hash = pending_live.mantle_tx.hash();
        state.submit_inscription(
            pending_live,
            config_id,
            MsgId::from([88u8; 32]),
            Inscription::new_unchecked(b"chained-from-config".to_vec()),
        );

        // The flow: extract_channel_tip_ops -> process_block. The cycle in
        // the current toposort surfaces here as a hang; the timeout wrapper
        // converts that into a clear failure.
        let extracted = run_with_timeout(std::time::Duration::from_secs(2), move || {
            extract_channel_tip_ops(std::slice::from_ref(&tx), channel_id)
        });
        state.process_block(block, genesis, genesis, vec![tx_hash], extracted);

        // Tip must match the ledger after the replay.
        assert_eq!(
            state.channel_tip_at(block),
            config_id,
            "channel tip after same-block replay must equal hash(X)"
        );

        // shed_off_branch_pending should drop the stale one but not the live one.
        let shed = state.shed_off_branch_pending(block);
        let shed_hashes: std::collections::HashSet<TxHash> =
            shed.iter().map(PendingTx::tx_hash).collect();
        assert!(
            shed_hashes.contains(&pending_stale_hash),
            "pending chained from I1 must be shed (no longer reachable from tip)"
        );
        assert!(
            !shed_hashes.contains(&pending_live_hash),
            "pending chained from the config tip must remain on-branch"
        );
    }

    #[test]
    fn same_block_config_replay_adjacent_yields_correct_tip() {
        // Simpler shape: [ChannelConfig X, ChannelConfig X] in one block.
        // No cycle in the current toposort (parent keys are distinct because
        // last_in_block differs) but exercises the duplicate-this_msg path.
        let channel_id = ChannelId::from([0u8; 32]);
        let config = channel_config(channel_id);
        let config_id = config.id();

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelConfig(config.clone()),
            Op::ChannelConfig(config),
        ]);
        let tx_hash = tx.mantle_tx.hash();

        let genesis = header_id(0);
        let block = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let extracted = run_with_timeout(std::time::Duration::from_secs(2), move || {
            extract_channel_tip_ops(std::slice::from_ref(&tx), channel_id)
        });
        state.process_block(block, genesis, genesis, vec![tx_hash], extracted);

        assert_eq!(state.channel_tip_at(block), config_id);
    }

    #[test]
    fn cross_block_config_replay_after_lib_advance_yields_correct_tip() {
        // Case 1 across LIB-advance:
        //   Block A: [ChannelConfig X]
        //   LIB advances past A — block_inscriptions for A is pruned;
        //                        finalized_msg becomes hash(X).
        //   Block B: [ChannelConfig X] (same content, re-applied)
        //
        // Two checks:
        //   - channel_tip_at(B) == config_id (tip stays correct after replay)
        //   - pending whose parent is finalized_msg=hash(X) stays on-branch, because
        //     the tip after B is still hash(X).
        let channel_id = ChannelId::from([0u8; 32]);
        let config = channel_config(channel_id);
        let config_id = config.id();

        let tx_a = unverified_tx_with_ops(vec![Op::ChannelConfig(config.clone())]);
        let tx_a_hash = tx_a.mantle_tx.hash();
        let tx_b = unverified_tx_with_ops(vec![Op::ChannelConfig(config)]);
        let tx_b_hash = tx_b.mantle_tx.hash();

        let genesis = header_id(0);
        let block_a = header_id(1);
        let block_b = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        // Process block A; LIB stays at genesis.
        let extracted_a = extract_channel_tip_ops(std::slice::from_ref(&tx_a), channel_id);
        state.process_block(block_a, genesis, genesis, vec![tx_a_hash], extracted_a);
        assert_eq!(state.channel_tip_at(block_a), config_id);

        // A dummy intermediate block to advance LIB past A. LIB advance to
        // block_a finalizes the config and prunes A's block_inscriptions.
        let block_intermediate = header_id(3);
        state.process_block(block_intermediate, block_a, block_a, vec![], vec![]);

        // Pending chained from the now-finalized config tip.
        let pending = dummy_pending_tx(5);
        let pending_hash = pending.mantle_tx.hash();
        state.submit_inscription(
            pending,
            config_id,
            MsgId::from([77u8; 32]),
            Inscription::new_unchecked(b"after-finalized".to_vec()),
        );

        // Process block B carrying the replay.
        let extracted_b = extract_channel_tip_ops(std::slice::from_ref(&tx_b), channel_id);
        state.process_block(
            block_b,
            block_intermediate,
            block_a,
            vec![tx_b_hash],
            extracted_b,
        );

        // Tip after the replay is still hash(X) — matches the ledger.
        assert_eq!(
            state.channel_tip_at(block_b),
            config_id,
            "channel tip after cross-block replay must equal hash(X)"
        );

        // Our pending's parent matches the new tip, so it should stay.
        let shed = state.shed_off_branch_pending(block_b);
        let shed_hashes: std::collections::HashSet<TxHash> =
            shed.iter().map(PendingTx::tx_hash).collect();
        assert!(
            !shed_hashes.contains(&pending_hash),
            "pending chained from the (still-current) config tip must remain on-branch"
        );
    }
}
