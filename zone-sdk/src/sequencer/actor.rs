#![allow(
    clippy::multiple_inherent_impl,
    reason = "`ZoneSequencer`'s public API lives in zone_sequencer.rs; internal handlers live here."
)]

use std::collections::HashSet;

use lb_common_http_client::{ProcessedBlockEvent, Slot};
use lb_core::mantle::channel::ChannelState;
use tracing::{debug, error, warn};

use super::{
    TARGET,
    block_fetch::{BlockEventResult, handle_block_event, orphan_from_shed},
    slot_clock::{SlotClock, slot_to_u64},
    state::{ChannelUpdateInfo, TxState},
    types::{
        ChannelUpdate, Error, Event, FinalizedTx, InscriptionInfo, OrphanedTx,
        SequencerChannelView, SequencerCheckpoint, TurnNotification, TxSource, TxStatus,
    },
    zone_sequencer::{ZoneSequencer, build_checkpoint},
};
use crate::adapter;

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Handle a single item from the blocks stream. `None` means the stream
    /// disconnected; any other value is processed as a block event and
    /// produces an [`Event::BlocksProcessed`] carrying the checkpoint, the
    /// optional `ChannelUpdate`, and the block's finalized txs.
    pub(super) async fn handle_stream_item(
        &mut self,
        maybe_event: Option<ProcessedBlockEvent>,
    ) -> Option<Event> {
        let Some(block_event) = maybe_event else {
            warn!(target: TARGET, "Blocks stream disconnected, will reconnect on next call");
            self.blocks_stream = None;
            self.handle_stream_drop();
            return None;
        };

        if let Ok(result) = self.process_block_event(&block_event).await {
            self.finish_block_processing(result)
        } else {
            self.handle_stream_drop();
            None
        }
    }

    /// Ingest one live block event into local state. On any per-block error
    /// (block processing, channel-state refresh) the stream is dropped so
    /// the reconnect path retries the same event, and `Err(())` is returned
    /// — the caller maps that to `handle_stream_drop`.
    async fn process_block_event(
        &mut self,
        block_event: &ProcessedBlockEvent,
    ) -> Result<BlockEventResult, ()> {
        let result = handle_block_event(
            block_event,
            &mut self.state,
            &mut self.current_tip,
            &mut self.lib_slot,
            self.channel_id,
            &self.node,
        )
        .await
        .map_err(|e| {
            error!(
                target: TARGET,
                "Block event processing failed; dropping stream so reconnect retries: {e}"
            );
            self.blocks_stream = None;
        })?;

        if let Some(slot_clock) = self.slot_clock.as_mut() {
            slot_clock.observe_slot(block_event.tip_slot);
        }

        self.refresh_channel_state().await.map_err(|err| {
            error!(
                target: TARGET,
                "Failed to refresh channel state after block; dropping stream so reconnect retries: {err}"
            );
            self.blocks_stream = None;
        })?;

        Ok(result)
    }

    /// Convert a successfully-ingested block into the public event. Handles
    /// the readiness-transition special case: when this is the block that
    /// flips the sequencer to ready, emit `Ready` first and buffer the
    /// `BlocksProcessed` for the next drive turn.
    fn finish_block_processing(&mut self, result: BlockEventResult) -> Option<Event> {
        // We just processed a live block end-to-end — cached `channel_state`,
        // `current_tip`, and `lib_slot` reflect chain state up to this block,
        // so callers may rely on them.
        self.connected = true;
        let became_ready = self.maybe_signal_ready();
        let (channel_update, finalized, mined) = self.apply_block_result(result);

        // Failed posts (still `!posted`) get retried by the turn-change
        // handler and the `resubmit_interval` self-heal tick. Don't queue
        // unconditionally on every block.
        //
        // Refresh the channel view so `our_turn_to_write` re-evaluates
        // against the just-advanced slot clock — the turn-change handler
        // inside relies on this to fire `resubmit_pending` when our turn
        // arrives.
        self.publish_channel_view();

        self.queue_block_status_events(&channel_update, &finalized, &mined);

        let block_event = self
            .publish_checkpoint()
            .map(|checkpoint| Event::BlocksProcessed {
                checkpoint,
                channel_update,
                finalized,
            });

        if became_ready {
            if let Some(ev) = block_event {
                self.buffered_events.push_back(ev);
            }
            return Some(self.emit_now(Event::Ready));
        }

        if let Some(ev) = block_event {
            self.buffered_events.push_back(ev);
        }
        self.buffered_events.pop_front()
    }

    /// If not yet ready and startup backfill is complete, mark ready. Returns
    /// true if readiness transitioned.
    fn maybe_signal_ready(&self) -> bool {
        if self.is_ready() {
            return false;
        }

        if self.backfill_from.is_none() && self.backfill_to.is_none() {
            debug!(target: TARGET, "Sequencer ready (backfill complete, first block processed)");
            self.ready_tx.send_replace(true);
            true
        } else {
            debug!(target: TARGET,
                "Not yet ready: backfill_from={:?}, backfill_to={:?}",
                self.backfill_from, self.backfill_to
            );
            false
        }
    }

    /// Bookkeeping for a stream drop: clears `connected` so operations that
    /// depend on cached on-chain state (inscription turn check, atomic
    /// withdraw nonce, channel config) fail-fast with `Error::Unavailable`
    /// rather than building txs from stale state. Also clears turn-to-write
    /// so consumers observing the watch don't see a stale "our turn" while
    /// disconnected. Readiness stays latched true after the first cold-start
    /// completion — in-memory state remains valid, and any tx invalidated
    /// during the disconnect surfaces as an orphan on the next
    /// `BlocksProcessed` once the stream resumes.
    fn handle_stream_drop(&mut self) {
        self.connected = false;
        self.publish_turn_to_write(false);
    }

    /// Build the current checkpoint from internal state and publish it to the
    /// `checkpoint_tx` watch channel. Returns the built checkpoint (or `None`
    /// if state isn't initialised yet) so callers can reuse it to construct
    /// the matching [`Event::BlocksProcessed`].
    pub(super) fn publish_checkpoint(&self) -> Option<SequencerCheckpoint> {
        let checkpoint = self
            .state
            .as_ref()
            .map(|s| build_checkpoint(s, self.last_msg_id, self.lib_slot));
        // `send_replace` (not `send`) so the stored value updates even when
        // there are no subscribers — `ZoneSequencer::checkpoint()` reads the
        // stored value directly via `borrow()`.
        self.checkpoint_tx.send_replace(checkpoint.clone());
        checkpoint
    }

    pub(super) async fn refresh_channel_state(&mut self) -> Result<(), Error> {
        let channel = self
            .node
            .channel_state(self.channel_id)
            .await
            .map_err(|err| Error::Network(err.to_string()))?;
        self.own_key_index = channel
            .as_ref()
            .and_then(|channel| self.own_key_index_for(channel));
        self.channel_state = channel;
        Ok(())
    }

    fn channel_view(&self) -> SequencerChannelView {
        let current_slot = self
            .slot_clock
            .as_ref()
            .map_or(Slot::genesis(), SlotClock::current_slot);

        let authorized_key_index = self
            .channel_state
            .as_ref()
            .map(|channel| channel.round_robin(current_slot).0);

        let tip_message = self
            .channel_state
            .as_ref()
            .map_or(self.last_msg_id, |channel| channel.tip_message);

        let posting_timeframe = self
            .channel_state
            .as_ref()
            .map(|channel| u32::from(channel.posting_timeframe.clone()));

        let posting_timeout = self
            .channel_state
            .as_ref()
            .map(|channel| u32::from(channel.posting_timeout.clone()));

        let accredited_key_count = self
            .channel_state
            .as_ref()
            .map(|channel| channel.accredited_keys.len());

        let pending_publish_txs = self
            .state
            .as_ref()
            .map_or(0, TxState::pending_publish_count);

        SequencerChannelView {
            channel_id: self.channel_id,
            channel: self.channel_state.clone(),
            current_slot,
            own_key_index: self.own_key_index,
            authorized_key_index,
            our_turn_to_write: self.can_publish_inscription_now(),
            tip_message,
            pending_publish_txs,
            queued_messages: pending_publish_txs,
            turn_to_write_slots: posting_timeframe,
            posting_timeout_slots: posting_timeout,
            accredited_key_count,
        }
    }

    pub(super) fn publish_channel_view(&mut self) {
        let view = self.channel_view();
        let turn_to_write = self.is_ready() && view.our_turn_to_write;
        // `send_replace` so the stored value stays current even with no
        // subscribers (sync reads happen via late `subscribe_channel_view`).
        self.channel_view_tx.send_replace(view);
        self.publish_turn_to_write(turn_to_write);
    }

    fn publish_turn_to_write(&mut self, turn_to_write: bool) {
        let mut emitted: Option<TurnNotification> = None;
        let mut became_our_turn = false;

        self.turn_to_write_tx.send_if_modified(|current| {
            let new = self.turn_notification(turn_to_write);
            let changed = current.our_turn_to_write != new.our_turn_to_write
                || current.starting_slot != new.starting_slot
                || current.ends_at_slot != new.ends_at_slot
                || current.turn_to_write_slots != new.turn_to_write_slots;

            became_our_turn = !current.our_turn_to_write && new.our_turn_to_write;
            *current = new.clone();
            if changed {
                emitted = Some(new);
            }

            changed
        });

        if became_our_turn {
            // Drain whatever accumulated while not-our-turn (turn-gated
            // publishes were skipped) and refresh mempool for any posted
            // tx that may have been evicted. Idempotent via mempool dedup.
            self.resubmit_pending();
        }
        if let Some(notification) = emitted {
            drop(self.event_tx.send(Event::TurnNotification { notification }));
        }
    }

    fn turn_notification(&self, our_turn_to_write: bool) -> TurnNotification {
        let Some(slot_clock) = &self.slot_clock else {
            return TurnNotification {
                our_turn_to_write,
                starting_slot: None,
                ends_at_slot: None,
                turn_to_write_slots: None,
                current_slot: None,
            };
        };

        let current_slot = slot_clock.current_slot();
        let Some(channel) = &self.channel_state else {
            return TurnNotification {
                our_turn_to_write,
                starting_slot: None,
                ends_at_slot: None,
                turn_to_write_slots: None,
                current_slot: Some(slot_to_u64(current_slot)),
            };
        };

        let (_, turn_start_slot) = channel.round_robin(current_slot);
        let turn_to_write_slots = u32::from(channel.posting_timeframe.clone());
        let starting_slot = slot_to_u64(turn_start_slot);
        let ends_at_slot = starting_slot.saturating_add(u64::from(turn_to_write_slots));

        TurnNotification {
            our_turn_to_write,
            starting_slot: Some(starting_slot),
            ends_at_slot: Some(ends_at_slot),
            turn_to_write_slots: Some(turn_to_write_slots),
            current_slot: Some(slot_to_u64(current_slot)),
        }
    }

    fn own_key_index_for(&self, channel: &ChannelState) -> Option<u16> {
        channel
            .accredited_keys
            .iter()
            .position(|pk| *pk == self.signing_key.public_key())
            .map(|idx| idx as u16)
    }

    pub(super) fn can_publish_inscription_now(&self) -> bool {
        if !self.connected {
            return false;
        }
        let Some(slot_clock) = &self.slot_clock else {
            return false;
        };
        let current_slot = slot_clock.current_slot();

        let Some(channel) = &self.channel_state else {
            // A missing channel is the normal pre-genesis-inscription state.
            // Network/query failures are surfaced before this point, so this
            // still only publishes when the absence is known.
            return true;
        };

        let Some(own_idx) = self.own_key_index else {
            return false;
        };

        let (authorized_idx, turn_start_slot) = channel.round_robin(current_slot);
        authorized_idx == own_idx
            && self.has_enough_turn_time_left(channel, current_slot, turn_start_slot)
    }

    fn has_enough_turn_time_left(
        &self,
        channel: &ChannelState,
        current_slot: Slot,
        turn_start_slot: Slot,
    ) -> bool {
        let min_remaining = self.config.min_slots_remaining_in_turn;
        let posting_timeframe = u32::from(channel.posting_timeframe.clone());
        if min_remaining == 0 || posting_timeframe == 0 {
            return true;
        }

        let turn_end_slot =
            slot_to_u64(turn_start_slot).saturating_add(u64::from(posting_timeframe));
        turn_end_slot.saturating_sub(slot_to_u64(current_slot)) >= min_remaining
    }

    /// Re-post pending txs that aren't safe at the current tip by pushing
    /// a `post_transaction` batch into `in_flight_resubmit`. The drive
    /// loop's `next_event` arm drains it and marks successful posts;
    /// failures stay unposted for the next tick. Inscription publishes
    /// are gated by the round-robin window; first-time posts are bounded
    /// by `max_pending_publish_depth`.
    ///
    /// Skips if a previous broad sweep is still in flight — guards against
    /// the 30s timer + turn-change handler firing close together and
    /// producing duplicate POSTs for the same pending set.
    pub(super) fn resubmit_pending(&mut self) {
        if self
            .resubmit_active
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            debug!(target: TARGET, "Skipping resubmit; previous broad sweep still in flight");
            self.publish_channel_view();
            return;
        }

        let Some(tip) = self.current_tip else {
            self.publish_channel_view();
            return;
        };

        let submit = {
            let Some(state) = self.state.as_ref() else {
                self.publish_channel_view();
                return;
            };

            let can_publish_inscription = self.can_publish_inscription_now();
            let pending = state.pending_txs(tip);
            let max_depth = self.config.max_pending_publish_depth.max(1);

            let mut submit = Vec::new();
            let mut active_publish_count = state.posted_pending_publish_count();

            for (id, signed_tx) in pending {
                // Skip txs whose post is already in flight — guards the
                // publish↔resubmit race that would otherwise re-queue the
                // same tx while its first `post_batch` is still running.
                if self.posting.contains(&id) {
                    continue;
                }

                let pending_inscription_publish = state.pending_inscription(&id);
                let is_inscription_publish = pending_inscription_publish.is_some();
                if is_inscription_publish && !can_publish_inscription {
                    continue;
                }

                let is_first_inscription_post =
                    pending_inscription_publish.is_some_and(|pending| !pending.posted);
                if is_inscription_publish
                    && is_first_inscription_post
                    && active_publish_count >= max_depth
                {
                    break;
                }

                if is_inscription_publish && is_first_inscription_post {
                    active_publish_count = active_publish_count.saturating_add(1);
                }
                submit.push((id, signed_tx));
            }
            submit
        };

        if submit.is_empty() {
            self.publish_channel_view();
            return;
        }

        debug!(target: TARGET, "Queueing {} pending transaction(s) for resubmit", submit.len());
        self.queue_resubmit_batch(submit);

        self.publish_channel_view();
    }

    /// Process a `BlockEventResult`: apply channel updates to local state and
    /// return the resulting channel-update + finalized-tx delta. When the tip
    /// did not change, returns an empty [`ChannelUpdate`] (both vecs empty) —
    /// internally we still skip the orphan/adopted computation in that case
    /// via `block_fetch::handle_block_event`'s `Option` short-circuit.
    fn apply_block_result(
        &mut self,
        result: BlockEventResult,
    ) -> (ChannelUpdate, Vec<FinalizedTx>, Vec<InscriptionInfo>) {
        let channel_update = match result.channel_update {
            Some(update) => {
                Self::log_channel_update(&update);

                let has_pending = self
                    .state
                    .as_ref()
                    .is_some_and(TxState::has_pending_inscriptions);

                if !update.orphaned.is_empty() || !has_pending {
                    self.last_msg_id = update.new_channel_tip;
                }

                self.build_channel_update(update)
            }
            None => ChannelUpdate {
                orphaned: Vec::new(),
                adopted: Vec::new(),
            },
        };
        (
            channel_update,
            result.finalized_items,
            result.mined_inscriptions,
        )
    }

    fn queue_block_status_events(
        &mut self,
        channel_update: &ChannelUpdate,
        finalized: &[FinalizedTx],
        mined: &[InscriptionInfo],
    ) {
        for tx in &channel_update.orphaned {
            let tx_hash = tx.tx_hash();
            let source = self
                .state
                .as_ref()
                .map_or(TxSource::Other, |state| state.tx_source(&tx_hash));
            self.queue_tx_status(tx_hash, TxStatus::Orphaned(source));
        }
        // `OnChain` is a per-tx lifecycle signal — it fires when an inscription
        // lands in a block, independent of whether it moved the channel lineage.
        // Our own publishes are already in the lineage, so they never appear in
        // `adopted`; drive `OnChain` from what was actually mined this block.
        for info in mined {
            let source = self
                .state
                .as_ref()
                .map_or(TxSource::Other, |state| state.tx_source(&info.tx_hash));
            self.queue_tx_status(info.tx_hash, TxStatus::OnChain(source));
        }
        for tx in finalized {
            let source = self
                .state
                .as_ref()
                .map_or(TxSource::Other, |state| state.tx_source(&tx.tx_hash));
            self.queue_tx_status(tx.tx_hash, TxStatus::Finalized(source));
        }
    }

    fn log_channel_update(update: &ChannelUpdateInfo) {
        debug!(target: TARGET,
            "ChannelUpdate: orphaned={}, adopted={}, new_tip={}",
            update.orphaned.len(),
            update.adopted.len(),
            hex::encode(update.new_channel_tip.as_ref()),
        );
        for info in &update.orphaned {
            debug!(target: TARGET,
                "  orphaned: payload={:?}, tx={}, msg_id={}",
                String::from_utf8_lossy(&info.payload),
                hex::encode(info.tx_hash.0),
                hex::encode(info.this_msg.as_ref()),
            );
        }
        for info in &update.adopted {
            debug!(target: TARGET,
                "  adopted: payload={:?}, tx={}, msg_id={}",
                String::from_utf8_lossy(&info.payload),
                hex::encode(info.tx_hash.0),
                hex::encode(info.this_msg.as_ref()),
            );
        }
    }

    /// Build the [`ChannelUpdate`] returned to the consumer.
    ///
    /// `orphaned` combines two sources, deduped by `tx_hash`:
    /// - inscriptions that left the channel chain between the old and new
    ///   canonical tip, and
    /// - our own pending that can no longer land on the new tip
    ///   ([`TxState::shed_off_branch_pending`]), including pending that never
    ///   mined and so appears in no on-chain delta.
    ///
    /// A tx in both keeps the shed variant: it carries the `AtomicWithdraw`
    /// bundle metadata the on-chain delta lacks.
    ///
    /// `adopted` is the inscriptions added to the channel chain.
    fn build_channel_update(&mut self, u: ChannelUpdateInfo) -> ChannelUpdate {
        let shed = match (self.state.as_mut(), self.current_tip) {
            (Some(s), Some(tip)) => s.shed_off_branch_pending(tip),
            _ => Vec::new(),
        };
        let mut orphaned: Vec<OrphanedTx> = shed.into_iter().map(orphan_from_shed).collect();

        let mut seen: HashSet<_> = orphaned.iter().map(OrphanedTx::tx_hash).collect();
        for info in u.orphaned {
            if seen.insert(info.tx_hash) {
                orphaned.push(OrphanedTx::Inscription(info));
            }
        }

        ChannelUpdate {
            orphaned,
            adopted: u.adopted,
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use futures::StreamExt as _;
    use lb_common_http_client::{
        ApiBlock, ApiHeader, BlockInfo, ChainServiceInfo, ChainServiceMode, CryptarchiaInfo, State,
        TimeInfo,
    };
    use lb_core::{
        header::{ContentId, HeaderId},
        mantle::{
            MantleTx, Note, Op, SignedMantleTx, Transaction as _, Utxo,
            ledger::{Inputs, Outputs},
            ops::{
                OpProof,
                channel::{
                    ChannelId, MsgId,
                    config::Keys,
                    deposit::DepositOp,
                    inscribe::{Inscription, InscriptionOp},
                    withdraw::ChannelWithdrawOp,
                },
            },
            transactions::Ops,
        },
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_http_api_common::queries::BlocksStreamQuery;
    use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature, ZkKey};
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};
    use tokio::sync::{mpsc, watch};

    use super::{
        super::{
            types::{FinalizedOp, SequencerConfig},
            zone_sequencer::restore_pending_tx,
        },
        *,
    };
    use crate::{ZoneMessage, adapter::BoxStream};

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

    #[must_use]
    pub fn utxo_with_sk() -> (ZkKey, Utxo) {
        let mut op_id = [0u8; 32];
        thread_rng().fill_bytes(&mut op_id);
        let zk_sk = ZkKey::from(BigUint::from(0u64));
        let utxo = Utxo {
            op_id,
            output_index: 0,
            note: Note::new(10, zk_sk.to_public_key()),
        };

        (zk_sk, utxo)
    }

    #[tokio::test]
    async fn prepare_submit_deposit_and_inscription() {
        // Init a sequencer
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (node, mut posted_txs) = MockNode::new();
        let mut sequencer = ZoneSequencer::init(channel_id, sequencer_key, node, None);

        // Drive sequencer until ready
        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        // Prepare a deposit op
        let (sk, utxo) = utxo_with_sk();
        let deposit_op = DepositOp {
            channel_id,
            inputs: Inputs::new([utxo.id()]),
            metadata: b"to Alice".into(),
        };

        // Build a `MantleTx` via the handle
        let (tx, msg_id, inscription_sig) = sequencer
            .handle()
            .prepare_tx(
                [Op::ChannelDeposit(deposit_op.clone())].into(),
                b"Mint 10 to Alice".into(),
            )
            .unwrap();
        assert_eq!(tx.ops().len(), 2);
        assert_eq!(&tx.ops()[0], &Op::ChannelDeposit(deposit_op));
        assert!(matches!(&tx.ops()[1], &Op::ChannelInscribe(_)));

        // Sign the `MantleTx`
        let signed_tx = SignedMantleTx::new(
            tx.clone(),
            vec![
                OpProof::ZkSig(
                    ZkKey::multi_sign(std::slice::from_ref(&sk), &tx.clone().hash().to_fr())
                        .unwrap(),
                ),
                OpProof::Ed25519Sig(inscription_sig),
            ],
        )
        .unwrap();

        // Submit via the handle (mutates state + queues post to in_flight).
        let (result, checkpoint) = sequencer
            .handle()
            .submit_signed_tx(signed_tx.clone(), msg_id)
            .unwrap();
        assert_eq!(result.inscription_id(), signed_tx.mantle_tx.hash());
        assert_eq!(checkpoint.last_msg_id, msg_id);

        // The post lives in `in_flight` until the drive loop polls it.
        // Drive `next_event` concurrently with the recv so the post future
        // runs and MockNode delivers to `posted_txs`.
        tokio::select! {
            tx = posted_txs.recv() => assert_eq!(tx.unwrap(), signed_tx),
            () = async {
                loop {
                    drop(sequencer.next_event().await);
                }
            } => unreachable!(),
        }
    }

    #[derive(Clone)]
    struct MockNode {
        posted_transactions_sender: mpsc::Sender<SignedMantleTx>,
        channel_state: Option<ChannelState>,
    }

    impl MockNode {
        fn new() -> (Self, mpsc::Receiver<SignedMantleTx>) {
            let (tx, rx) = mpsc::channel(10);
            (
                Self {
                    posted_transactions_sender: tx,
                    channel_state: Some(ChannelState {
                        accredited_keys: Keys::from(Ed25519Key::from_bytes(&[0; 32]).public_key())
                            .into(),
                        configuration_threshold: 1,
                        tip_message: MsgId::root(),
                        tip_slot: Slot::default(),
                        tip_sequencer: 0,
                        tip_sequencer_starting_slot: Slot::default(),
                        posting_timeframe: 0u32.into(),
                        posting_timeout: 0u32.into(),
                        balance: 0,
                        withdrawal_nonce: 0,
                        withdraw_threshold: 1,
                    }),
                },
                rx,
            )
        }
    }

    /// Like [`MockNode`], but with a controllable connectivity flag.
    ///
    /// Used to exercise reconnect behavior. While "down" (`up` set to `false`),
    /// `block_stream` errors and any previously-returned live stream ends, so
    /// the sequencer notices the disconnect and re-enters `ensure_connected`.
    #[derive(Clone)]
    struct ReconnectMockNode {
        inner: MockNode,
        up_rx: watch::Receiver<bool>,
    }

    impl ReconnectMockNode {
        fn new() -> (Self, mpsc::Receiver<SignedMantleTx>, watch::Sender<bool>) {
            let (inner, posted_rx) = MockNode::new();
            let (up_tx, up_rx) = watch::channel(true);
            (Self { inner, up_rx }, posted_rx, up_tx)
        }
    }

    #[async_trait]
    impl adapter::Node for ReconnectMockNode {
        async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
            self.inner.consensus_info().await
        }

        async fn time_info(&self) -> Result<TimeInfo, lb_common_http_client::Error> {
            self.inner.time_info().await
        }

        async fn channel_state(
            &self,
            channel_id: ChannelId,
        ) -> Result<Option<ChannelState>, lb_common_http_client::Error> {
            self.inner.channel_state(channel_id).await
        }

        async fn block_stream(
            &self,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            if !*self.up_rx.borrow() {
                return Err(lb_common_http_client::Error::Client("node down".to_owned()));
            }
            let initial = futures::stream::once(async {
                ProcessedBlockEvent {
                    block: ApiBlock {
                        header: ApiHeader {
                            id: HeaderId::from([1; 32]),
                            parent_block: HeaderId::from([0; 32]),
                            slot: 1.into(),
                            block_root: ContentId::from([0; 32]),
                            proof_of_leadership: Groth16LeaderProof::genesis(),
                        },
                        transactions: Vec::new(),
                    },
                    tip: HeaderId::from([1; 32]),
                    tip_slot: 1.into(),
                    lib: HeaderId::from([0; 32]),
                    lib_slot: Slot::genesis(),
                }
            });
            // Stay open until the node goes down, then end so the sequencer
            // re-enters `ensure_connected` (where `block_stream` errors).
            let up_rx = self.up_rx.clone();
            let until_down = futures::stream::once(async move {
                let mut up_rx = up_rx;
                while *up_rx.borrow_and_update() {
                    if up_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .filter_map(async |()| None::<ProcessedBlockEvent>);
            Ok(Box::pin(initial.chain(until_down)))
        }

        async fn blocks_range_stream(
            &self,
            params: BlocksStreamQuery,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            self.inner.blocks_range_stream(params).await
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
            self.inner.lib_stream().await
        }

        async fn block(
            &self,
            id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            self.inner.block(id).await
        }

        async fn block_events(
            &self,
            id: HeaderId,
        ) -> Result<Option<lb_common_http_client::Events>, lb_common_http_client::Error> {
            self.inner.block_events(id).await
        }

        async fn immutable_blocks(
            &self,
            slot_from: Slot,
            slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
            self.inner.immutable_blocks(slot_from, slot_to).await
        }

        async fn zone_messages_in_block(
            &self,
            id: HeaderId,
            channel_id: ChannelId,
        ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            self.inner.zone_messages_in_block(id, channel_id).await
        }

        async fn zone_messages_in_blocks(
            &self,
            slot_from: Slot,
            slot_to: Slot,
            channel_id: ChannelId,
        ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
            self.inner
                .zone_messages_in_blocks(slot_from, slot_to, channel_id)
                .await
        }

        async fn post_transaction(
            &self,
            tx: SignedMantleTx,
        ) -> Result<(), lb_common_http_client::Error> {
            self.inner.post_transaction(tx).await
        }
    }

    /// Comment #1 regression guard for client publishes during reconnect.
    ///
    /// A `SequencerClient::publish` issued while the node is down (reconnect in
    /// progress) must be accepted locally and resolve promptly — matching
    /// `SequencerHandle::publish` — instead of blocking until connectivity is
    /// restored. It must also be posted once the node comes back.
    #[tokio::test]
    async fn client_publish_accepted_locally_during_reconnect() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (node, mut posted_txs, up_tx) = ReconnectMockNode::new();
        let config = SequencerConfig {
            reconnect_delay: std::time::Duration::from_millis(20),
            resubmit_interval: std::time::Duration::from_millis(20),
            ..SequencerConfig::default()
        };
        let mut sequencer =
            ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
        let client = sequencer.client();

        // Drive until the sequencer has emitted `Ready`.
        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        // Take the node down: the live stream ends and the sequencer enters
        // reconnect (subsequent `block_stream` calls error).
        up_tx.send(false).unwrap();

        // A client publish while the node is down must resolve promptly. We
        // drive `next_event` concurrently; the publish is serviced from inside
        // `wait_reconnect_delay` while the node is still down. With the old
        // behavior the request would never be drained during reconnect and this
        // would hang (caught by the timeout).
        let publish = client.publish(b"during-reconnect".into());
        let (_result, _checkpoint) =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::select! {
                    result = publish => result,
                    () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
                }
            })
            .await
            .expect("client publish must resolve during reconnect, not block on connectivity")
            .expect("publish should be accepted locally after Ready");

        // Bring the node back up; the locally-queued inscription must be posted.
        up_tx.send(true).unwrap();
        let posted = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::select! {
                tx = posted_txs.recv() => tx,
                () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
            }
        })
        .await
        .expect("inscription should be posted after reconnect")
        .expect("posted_txs channel should be open");

        assert!(
            posted
                .mantle_tx
                .ops()
                .iter()
                .any(|op| matches!(op, Op::ChannelInscribe(_))),
            "posted tx should carry the inscription published during reconnect"
        );
    }

    #[test]
    fn restore_pending_tx_classifies_atomic_bundle_with_withdraws() {
        // Bundle: [ChannelWithdraw(channel_id), ChannelInscribe(channel_id)]
        // Restore should put it in pending (not pending_other) with the
        // withdraws field populated, so on orphan we emit
        // OrphanedTx::AtomicWithdraw (not Inscription).
        let channel_id = ChannelId::from([1u8; 32]);
        let outputs = Outputs::new([Note::new(
            5,
            ZkKey::from(BigUint::from(0u64)).to_public_key(),
        )]);
        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            outputs,
            withdraw_nonce: 0,
        };
        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };
        let mantle_tx = MantleTx(
            Ops::try_from(vec![
                Op::ChannelWithdraw(withdraw_op.clone()),
                Op::ChannelInscribe(inscribe_op),
            ])
            .unwrap(),
        );
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: Vec::new(),
        };

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        restore_pending_tx(&mut state, signed_tx, channel_id);

        let pending = state
            .pending_inscription(&tx_hash)
            .expect("bundle should be in pending inscriptions");
        let withdraws = pending
            .withdraws
            .as_ref()
            .expect("bundle should carry Some(withdraws)");
        assert_eq!(withdraws.len(), 1, "bundle should carry one WithdrawInfo");
        assert_eq!(withdraws[0].op, withdraw_op);
        assert!(
            !state.pending_other_contains(&tx_hash),
            "bundle should not be in pending_other"
        );
    }

    #[test]
    fn restore_pending_tx_classifies_plain_inscription_with_none_withdraws() {
        // Plain inscription: pending with `withdraws == None`.
        let channel_id = ChannelId::from([2u8; 32]);
        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };
        let mantle_tx = MantleTx(Ops::try_from(vec![Op::ChannelInscribe(inscribe_op)]).unwrap());
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: Vec::new(),
        };

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        restore_pending_tx(&mut state, signed_tx, channel_id);

        let pending = state
            .pending_inscription(&tx_hash)
            .expect("plain inscription should be in pending inscriptions");
        assert!(pending.withdraws.is_none());
    }

    #[test]
    fn restore_pending_tx_falls_back_to_other_when_no_inscribe_for_channel() {
        // Inscribe for a different channel: should fall back to pending_other
        // (treated as opaque).
        let our_channel = ChannelId::from([3u8; 32]);
        let other_channel = ChannelId::from([4u8; 32]);
        let inscribe_op = InscriptionOp {
            channel_id: other_channel,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };
        let mantle_tx = MantleTx(Ops::try_from(vec![Op::ChannelInscribe(inscribe_op)]).unwrap());
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: Vec::new(),
        };

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        restore_pending_tx(&mut state, signed_tx, our_channel);

        assert!(
            state.pending_inscription(&tx_hash).is_none(),
            "wrong-channel tx should not be in pending inscriptions"
        );
        assert!(
            state.pending_other_contains(&tx_hash),
            "wrong-channel tx should be in pending_other"
        );
    }

    #[async_trait]
    impl adapter::Node for MockNode {
        async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
            Ok(ChainServiceInfo {
                cryptarchia_info: CryptarchiaInfo {
                    lib: HeaderId::from([0; 32]),
                    lib_slot: Slot::genesis(),
                    tip: HeaderId::from([0; 32]),
                    slot: Slot::genesis(),
                    height: 0,
                },
                mode: ChainServiceMode::Started(State::Online),
            })
        }

        async fn time_info(&self) -> Result<TimeInfo, lb_common_http_client::Error> {
            Ok(TimeInfo {
                slot_duration_ms: 1_000,
                genesis_time_unix_ms: 0,
                current_slot: 0,
                current_epoch: 0,
            })
        }

        async fn channel_state(
            &self,
            _channel_id: ChannelId,
        ) -> Result<Option<ChannelState>, lb_common_http_client::Error> {
            Ok(self.channel_state.clone())
        }

        async fn block_stream(
            &self,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(
                futures::stream::once(async {
                    ProcessedBlockEvent {
                        block: ApiBlock {
                            header: ApiHeader {
                                id: HeaderId::from([1; 32]),
                                parent_block: HeaderId::from([0; 32]),
                                slot: 1.into(),
                                block_root: ContentId::from([0; 32]),
                                proof_of_leadership: Groth16LeaderProof::genesis(),
                            },
                            transactions: Vec::new(),
                        },
                        tip: HeaderId::from([1; 32]),
                        tip_slot: 1.into(),
                        lib: HeaderId::from([0; 32]),
                        lib_slot: Slot::genesis(),
                    }
                })
                .chain(futures::stream::pending()),
            ))
        }

        async fn blocks_range_stream(
            &self,
            _params: BlocksStreamQuery,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn block_events(
            &self,
            _id: HeaderId,
        ) -> Result<Option<lb_common_http_client::Events>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn immutable_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
            Ok(Vec::new())
        }

        async fn zone_messages_in_block(
            &self,
            _id: HeaderId,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn zone_messages_in_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn post_transaction(
            &self,
            tx: SignedMantleTx,
        ) -> Result<(), lb_common_http_client::Error> {
            self.posted_transactions_sender.send(tx).await.unwrap();
            Ok(())
        }
    }

    /// Mock node that serves a single genesis-slot block with a channel
    /// inscription, used to verify the cold-start backfill picks up slot 0.
    #[derive(Clone)]
    struct ColdStartMockNode {
        genesis_block: ApiBlock,
        live_block: ApiBlock,
        channel_state: Option<ChannelState>,
    }

    #[async_trait]
    impl adapter::Node for ColdStartMockNode {
        async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
            Ok(ChainServiceInfo {
                cryptarchia_info: CryptarchiaInfo {
                    lib: self.genesis_block.header.id,
                    lib_slot: Slot::genesis(),
                    tip: self.genesis_block.header.id,
                    slot: Slot::genesis(),
                    height: 0,
                },
                mode: ChainServiceMode::Started(State::Online),
            })
        }

        async fn time_info(&self) -> Result<TimeInfo, lb_common_http_client::Error> {
            Ok(TimeInfo {
                slot_duration_ms: 1_000,
                genesis_time_unix_ms: 0,
                current_slot: 0,
                current_epoch: 0,
            })
        }

        async fn block_stream(
            &self,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            let block = self.live_block.clone();
            let genesis_id = self.genesis_block.header.id;
            Ok(Box::pin(
                futures::stream::once(async move {
                    ProcessedBlockEvent {
                        block,
                        tip: HeaderId::from([2; 32]),
                        tip_slot: 1.into(),
                        lib: genesis_id,
                        lib_slot: Slot::genesis(),
                    }
                })
                .chain(futures::stream::pending()),
            ))
        }

        async fn blocks_range_stream(
            &self,
            _params: BlocksStreamQuery,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn block_events(
            &self,
            _id: HeaderId,
        ) -> Result<Option<lb_common_http_client::Events>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn immutable_blocks(
            &self,
            slot_from: Slot,
            slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
            // Cold-start backfill range is [0, 0] when lib_slot is genesis,
            // so we only return the genesis block for that exact range.
            if slot_from == Slot::genesis() && slot_to == Slot::genesis() {
                Ok(vec![self.genesis_block.clone()])
            } else {
                Ok(Vec::new())
            }
        }

        async fn zone_messages_in_block(
            &self,
            _id: HeaderId,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn zone_messages_in_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn post_transaction(
            &self,
            _tx: SignedMantleTx,
        ) -> Result<(), lb_common_http_client::Error> {
            Ok(())
        }

        async fn channel_state(
            &self,
            _channel_id: ChannelId,
        ) -> Result<Option<ChannelState>, lb_common_http_client::Error> {
            Ok(self.channel_state.clone())
        }
    }

    /// Cold start with a channel inscription at slot 0 (genesis): the
    /// sequencer must include that slot in its initial backfill and emit it
    /// in a `Finalized` state change. Regression guard for the off-by-one fix
    /// where `backfill_from = lib_slot + 1` silently skipped genesis.
    #[tokio::test]
    async fn cold_start_backfills_genesis_slot() {
        let channel_id = ChannelId::from([7; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

        // A signed tx with a single ChannelInscribe on our channel at
        // genesis (parent_msg = root).
        let inscribe = InscriptionOp {
            channel_id,
            parent: MsgId::root(),
            inscription: Inscription::new_unchecked(Vec::new()),
            signer: sequencer_key.public_key(),
        };
        let expected_msg_id = inscribe.id();
        let genesis_tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(inscribe)]);
        let genesis_tx_hash = genesis_tx.mantle_tx.hash();

        let genesis_block = ApiBlock {
            header: ApiHeader {
                id: HeaderId::from([1; 32]),
                parent_block: HeaderId::from([0; 32]),
                slot: Slot::genesis(),
                block_root: ContentId::from([0; 32]),
                proof_of_leadership: Groth16LeaderProof::genesis(),
            },
            transactions: vec![genesis_tx],
        };
        // Empty block at slot 1 so the block stream advances and the
        // sequencer signals `Ready`, giving the test a clean exit signal.
        let live_block = ApiBlock {
            header: ApiHeader {
                id: HeaderId::from([2; 32]),
                parent_block: HeaderId::from([1; 32]),
                slot: 1.into(),
                block_root: ContentId::from([0; 32]),
                proof_of_leadership: Groth16LeaderProof::genesis(),
            },
            transactions: Vec::new(),
        };

        let channel_state = Some(ChannelState {
            accredited_keys: Keys::from(Ed25519Key::from_bytes(&[0; 32]).public_key()).into(),
            configuration_threshold: 1,
            tip_message: MsgId::root(),
            tip_slot: Slot::default(),
            tip_sequencer: 0,
            tip_sequencer_starting_slot: Slot::default(),
            posting_timeframe: 0u32.into(),
            posting_timeout: 0u32.into(),
            balance: 0,
            withdrawal_nonce: 0,
            withdraw_threshold: 1,
        });

        let node = ColdStartMockNode {
            genesis_block,
            live_block,
            channel_state,
        };
        let mut sequencer = ZoneSequencer::init(channel_id, sequencer_key, node, None);

        let mut finalized_items: Vec<FinalizedTx> = Vec::new();
        loop {
            match sequencer.next_event().await {
                Event::Ready => break,
                Event::BlocksProcessed { finalized, .. } => {
                    finalized_items.extend(finalized);
                }
                Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
            }
        }

        assert_eq!(
            finalized_items.len(),
            1,
            "expected exactly one finalized tx from genesis backfill"
        );
        let t = &finalized_items[0];
        assert_eq!(t.tx_hash, genesis_tx_hash);
        assert_eq!(t.ops.len(), 1);
        match &t.ops[0] {
            FinalizedOp::Inscription(info) => {
                assert_eq!(info.tx_hash, genesis_tx_hash);
                assert_eq!(info.parent_msg, MsgId::root());
                assert_eq!(info.this_msg, expected_msg_id);
            }
            other => panic!("expected Inscription, got {other:?}"),
        }
    }
}
