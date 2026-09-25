#![allow(
    clippy::multiple_inherent_impl,
    reason = "`ZoneSequencer`'s public API lives in zone_sequencer.rs; internal handlers live here."
)]

use std::collections::HashSet;

use lb_common_http_client::{ProcessedBlockEvent, Slot};
use lb_core::mantle::{
    channel::ChannelState, ops::channel::ChannelId, traits::Hashable as _,
    transactions::hash::TxHash,
};
use tracing::{debug, error, warn};

use super::{
    TARGET,
    block_fetch::{BlockEventResult, classify_shed_other, handle_block_event, orphan_from_shed},
    slot_clock::{SlotClock, slot_to_u64},
    state::{ChannelUpdateInfo, TxState},
    types::{
        ChannelUpdate, ChannelUpdateTx, DepositInfo, Error, Event, FinalizedTx, InscriptionInfo,
        SequencerChannelView, SequencerCheckpoint, TurnNotification, TxSource, TxStatus,
    },
    zone_sequencer::{ZoneSequencer, build_checkpoint},
};
use crate::adapter;

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Process the block retained by the drive loop.
    ///
    /// The pending block is cleared only after processing completes. Dropping
    /// the surrounding [`ZoneSequencer::next_event`] future at an `.await`
    /// therefore leaves the block available for the next call.
    pub(super) async fn process_pending_block_event(&mut self) -> Option<Event> {
        let block_event = self
            .pending_block_event
            .clone()
            .expect("called only with a pending block event");

        if let Ok(result) = self.process_block_event(&block_event).await {
            self.pending_block_event = None;
            self.finish_block_processing(result)
                .map(|event| self.emit_now(event))
        } else {
            self.pending_block_event = None;
            self.handle_stream_drop();
            None
        }
    }

    pub(super) fn handle_stream_disconnect(&mut self) {
        warn!(target: TARGET, "Blocks stream disconnected, will reconnect on next call");
        self.blocks_stream = None;
        self.handle_stream_drop();
    }

    /// Ingest one live block event into local state. On any per-block error
    /// (block processing, channel-state refresh) the stream is dropped so
    /// the reconnect path retries the same event, and `Err(())` is returned
    /// — the caller maps that to `handle_stream_drop`.
    async fn process_block_event(
        &mut self,
        block_event: &ProcessedBlockEvent,
    ) -> Result<BlockEventResult, ()> {
        // Fetch first, then install the new channel state only after the block
        // has been processed. This avoids an `.await` after block state starts
        // changing, which is the unsafe cancellation boundary.
        let channel_state = fetch_channel_state(&self.node, self.channel_id)
            .await
            .map_err(|err| {
                error!(
                    target: TARGET,
                    "Failed to refresh channel state before block processing; dropping stream so reconnect retries: {err}"
                );
                self.blocks_stream = None;
            })?;

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

        self.install_channel_state(channel_state);

        Ok(result)
    }

    /// Convert a successfully-ingested block into the public event. Handles
    /// the readiness-transition special case: when this is the block that
    /// flips the sequencer to ready — or the one that completes a mid-life
    /// reconnect — emit `Ready` first and buffer the `BlocksProcessed` for
    /// the next drive turn.
    fn finish_block_processing(&mut self, result: BlockEventResult) -> Option<Event> {
        // We just processed a live block end-to-end — cached `channel_state`,
        // `current_tip`, and `lib_slot` reflect chain state up to this block,
        // so callers may rely on them.
        let reconnected = !self.connected;
        self.connected = true;
        let became_ready = self.maybe_signal_ready();
        let (channel_update, deposits, finalized, mined) = self.apply_block_result(result);

        self.queue_block_status_events(&channel_update, &finalized, &mined);

        let block_event = self
            .publish_checkpoint()
            .map(|checkpoint| Event::BlocksProcessed {
                checkpoint,
                channel_update,
                deposits,
                finalized,
            });
        if let Some(ev) = block_event {
            self.buffered_events.push_back(ev);
        }

        // Failed posts (still `!posted`) get retried by the turn-change
        // handler and the `resubmit_interval` self-heal tick. Don't queue
        // unconditionally on every block.
        //
        // Refresh the channel view so `our_turn_to_write` re-evaluates
        // against the just-advanced slot clock — the turn-change handler
        // inside relies on this to fire `resubmit_pending` when our turn
        // arrives. Runs after the block event is queued so a turn change
        // is reported after the block that caused it.
        self.publish_channel_view();

        // Re-announce readiness after a mid-life reconnect: with funding
        // configured, publishes fail fast with `Unavailable` while
        // disconnected, so consumers need a positive "you can publish again"
        // signal once a live block confirms the connection.
        if became_ready || (reconnected && self.is_ready()) {
            return Some(Event::Ready);
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

    fn install_channel_state(&mut self, channel: Option<ChannelState>) {
        self.own_key_index = channel
            .as_ref()
            .and_then(|channel| self.own_key_index_for(channel));
        self.channel_state = channel;
    }

    pub(super) async fn refresh_channel_state(&mut self) -> Result<(), Error> {
        let channel = fetch_channel_state(&self.node, self.channel_id).await?;
        self.install_channel_state(channel);

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
            self.buffered_events
                .push_back(Event::TurnNotification { notification });
        }
        self.turn_boundary = self.next_turn_wakeup();
    }

    /// The next slot at which `our_turn_to_write` can change: the round-robin
    /// boundary, or earlier the slot at which our turn has too little left to
    /// publish.
    fn next_turn_wakeup(&self) -> Option<Slot> {
        let slot_clock = self.slot_clock.as_ref()?;
        let channel = self.channel_state.as_ref()?;
        let current = slot_clock.current_slot();
        let rotation = channel.next_round_robin_boundary(current);
        let (authorized_idx, turn_start_slot) = channel.round_robin(current);
        let gate = self
            .turn_gate_closes_at(channel, turn_start_slot)
            .filter(|closes_at| self.own_key_index == Some(authorized_idx) && *closes_at > current);
        rotation.into_iter().chain(gate).min()
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
            .position(|pk| *pk == self.signing_key.public_key().into_unverified())
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
        self.turn_gate_closes_at(channel, turn_start_slot)
            .is_none_or(|closes_at| current_slot < closes_at)
    }

    /// First slot of the turn starting at `turn_start_slot` with fewer than
    /// `min_slots_remaining_in_turn` slots left; `None` when the margin is
    /// off or the turn is unbounded.
    fn turn_gate_closes_at(&self, channel: &ChannelState, turn_start_slot: Slot) -> Option<Slot> {
        let min_remaining = self.config.min_slots_remaining_in_turn;
        let posting_timeframe = u64::from(u32::from(channel.posting_timeframe.clone()));
        if min_remaining == 0 || posting_timeframe == 0 {
            return None;
        }
        let turn_end_slot = slot_to_u64(turn_start_slot).saturating_add(posting_timeframe);
        let closes_at = turn_end_slot
            .checked_sub(min_remaining)
            .map_or(0, |slot| slot.saturating_add(1));
        Some(Slot::from(closes_at))
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

    /// Turn a processed block into the consumer's [`ChannelUpdate`]: merge
    /// the shed passes into `orphaned`, then report an
    /// [`ChannelUpdate::Extension`] when nothing left the view and a
    /// [`ChannelUpdate::Conflict`] otherwise.
    fn apply_block_result(
        &mut self,
        result: BlockEventResult,
    ) -> (
        ChannelUpdate,
        Vec<DepositInfo>,
        Vec<FinalizedTx>,
        Vec<InscriptionInfo>,
    ) {
        let BlockEventResult {
            finalized_items,
            channel_update,
            common_prefix,
            mined_inscriptions,
            deposits,
        } = result;
        let (adopted, mut orphaned) = match channel_update {
            Some(update) => {
                Self::log_channel_update(&update);
                let ChannelUpdateInfo {
                    adopted, orphaned, ..
                } = update;
                let orphaned = self.shed_orphans(orphaned);

                // Advance the tip to the current valid publish parent (channel
                // tip + our pending tail), the same value publishing chains on.
                if let (Some(state), Some(tip)) = (self.state.as_ref(), self.current_tip) {
                    self.last_msg_id = state.publish_parent(tip);
                }

                (adopted, orphaned)
            }
            None => (Vec::new(), Vec::new()),
        };

        // Shed pending configs superseded on the config lineage; the lineage
        // diff already reports them orphaned.
        let stale_configs = match (self.state.as_mut(), self.current_tip) {
            (Some(s), Some(tip)) => s.shed_stale_pending_configs(tip),
            _ => Vec::new(),
        };
        let seen: HashSet<_> = orphaned.iter().map(ChannelUpdateTx::tx_hash).collect();
        for tx in stale_configs {
            if !seen.contains(&tx.hash()) {
                orphaned.push(classify_shed_other(tx, self.channel_id));
            }
        }

        // Shed the not-on-branch pending tail a config change may have
        // invalidated. Only never-mined entries are shed, so re-posts form at
        // most a competing branch that adoption collapses — never a duplicate.
        let config_shed = match (self.state.as_mut(), self.current_tip) {
            (Some(s), Some(tip)) => s.shed_pending_inscriptions_on_config(tip),
            _ => Vec::new(),
        };
        let config_shed_any = !config_shed.is_empty();
        let mut seen: HashSet<_> = orphaned.iter().map(ChannelUpdateTx::tx_hash).collect();
        for entry in config_shed {
            let tx = orphan_from_shed(entry);
            if seen.insert(tx.tx_hash()) {
                orphaned.push(tx);
            }
        }
        // Reset the chaining pointer to the message tip so re-posts re-home
        // there. This equals any reorg recovery tip, so the two don't conflict.
        if config_shed_any && let (Some(s), Some(tip)) = (self.state.as_ref(), self.current_tip) {
            self.last_msg_id = s.channel_tip_at(tip);
        }

        let channel_update = if orphaned.is_empty() {
            ChannelUpdate::Extension { adopted }
        } else {
            // The view was captured before the shed passes; whatever they
            // orphaned has left it.
            let shed: HashSet<TxHash> = orphaned.iter().map(ChannelUpdateTx::tx_hash).collect();
            let mut common_prefix = common_prefix;
            common_prefix.retain(|tx| !shed.contains(&tx.tx_hash()));
            ChannelUpdate::Conflict {
                common_prefix,
                adopted,
                orphaned,
            }
        };

        (
            channel_update,
            deposits,
            finalized_items,
            mined_inscriptions,
        )
    }

    fn queue_block_status_events(
        &mut self,
        channel_update: &ChannelUpdate,
        finalized: &[FinalizedTx],
        mined: &[InscriptionInfo],
    ) {
        for tx in channel_update.orphaned() {
            let tx_hash = tx.tx_hash();
            let source = self
                .state
                .as_ref()
                .map_or(TxSource::Other, |state| state.tx_source(&tx_hash));
            self.queue_tx_status(tx_hash, TxStatus::Orphaned(source));
        }
        // `OnChain` is a per-tx lifecycle signal — it fires when an inscription
        // lands in a block, independent of whether it moved the channel lineage.
        // Our own publishes never appear in extension-case `adopted` (already
        // tracked); drive `OnChain` from what was actually mined this block.
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
        for tx in &update.orphaned {
            Self::log_update_entry("orphaned", tx);
        }
        for tx in &update.adopted {
            Self::log_update_entry("adopted", tx);
        }
    }

    fn log_update_entry(kind: &str, tx: &ChannelUpdateTx) {
        if let Some(info) = tx.inscription() {
            debug!(target: TARGET,
                "  {kind}: payload={:?}, tx={}, msg_id={}",
                String::from_utf8_lossy(&info.payload),
                hex::encode(info.tx_hash.0),
                hex::encode(info.this_msg.as_ref()),
            );
        } else {
            debug!(target: TARGET,
                "  {kind}: custom tx {}",
                hex::encode(tx.tx_hash().0),
            );
        }
    }

    /// Merge the shed passes into the on-chain `orphaned` delta: bundles
    /// whose inputs left the branch (with their children), entries whose
    /// lineage no longer reaches the tip, and opaque txs the same way.
    /// Deduped by `tx_hash`.
    fn shed_orphans(&mut self, on_chain: Vec<ChannelUpdateTx>) -> Vec<ChannelUpdateTx> {
        let channel_id = self.channel_id;
        let (shed, shed_other) = match (self.state.as_mut(), self.current_tip) {
            (Some(s), Some(tip)) => {
                // Inputs shed first: it drains a dead bundle's children too.
                let mut shed = s.shed_bundles_with_missing_inputs(tip, channel_id);
                shed.extend(s.shed_off_branch_pending(tip));
                (shed, s.shed_off_branch_pending_other(tip))
            }
            _ => (Vec::new(), Vec::new()),
        };
        let mut orphaned: Vec<ChannelUpdateTx> = shed.into_iter().map(orphan_from_shed).collect();
        orphaned.extend(
            shed_other
                .into_iter()
                .map(|tx| classify_shed_other(tx, channel_id)),
        );

        let mut seen: HashSet<_> = orphaned.iter().map(ChannelUpdateTx::tx_hash).collect();
        for tx in on_chain {
            if seen.insert(tx.tx_hash()) {
                orphaned.push(tx);
            }
        }
        orphaned
    }
}

async fn fetch_channel_state<Node>(
    node: &Node,
    channel_id: ChannelId,
) -> Result<Option<ChannelState>, Error>
where
    Node: adapter::Node + Sync,
{
    node.channel_state(channel_id)
        .await
        .map_err(|err| Error::Network(err.to_string()))
}

#[cfg(test)]
mod tests {
    use lb_core::{
        header::HeaderId,
        mantle::{
            Note, Op, SignedOps, Utxo,
            channel::{SlotTimeframe, SlotTimeout},
            ledger::{Inputs, verification_mode::StandardMode},
            ops::{
                OpProof, OpProofRef, OpRef,
                channel::{
                    MsgId, UnverifiedChannelKeys, VerifiedChannelKeys,
                    config::ChannelConfigOp,
                    deposit::DepositOp,
                    inscribe::{Inscription, InscriptionOp},
                    withdraw::ChannelWithdrawOp,
                },
            },
            transactions::{OpProofs, Ops, states::Unverified},
        },
    };
    use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};
    use tokio::sync::{mpsc, watch};

    use super::{
        super::{
            state::PendingBundle,
            types::{FinalizedOp, SequencerConfig},
            zone_sequencer::track_pending_tx,
        },
        *,
    };
    use crate::test_support::{
        MockNode, StreamEnd, StreamScript, api_block, funding_config, header_id, live_event,
        scripts, single_key_channel_state, unverified_tx_with_ops,
    };

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
        let (node, mut posted_txs) = MockNode::with_posted_channel();
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

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
        assert_eq!(tx.inner().len(), 2);
        assert_eq!(tx.inner()[0], Op::ChannelDeposit(deposit_op));
        assert!(matches!(tx.inner()[1], Op::ChannelInscribe(_)));

        // Sign the `MantleTx`
        let op_proofs = OpProofs::from([
            OpProof::ZkSig(
                ZkKey::multi_sign(std::slice::from_ref(&sk), &tx.clone().hash().to_fr()).unwrap(),
            ),
            OpProof::Ed25519Sig(inscription_sig),
        ]);
        let signed_tx = SignedOps::from_parts(tx, op_proofs)
            .expect("Should generate a valid transaction with valid matching proofs.");

        // Submit via the handle (mutates state + queues post to in_flight).
        let (result, checkpoint) = sequencer
            .handle()
            .submit_signed_tx(signed_tx.clone(), msg_id)
            .unwrap();
        assert_eq!(result.inscription_id(), signed_tx.hash());
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

    /// A prepared tx pins the parent at prepare time; a publish in between
    /// takes that position, so the stale tx is refused instead of competing.
    #[tokio::test]
    async fn submit_signed_tx_refuses_a_parent_with_a_pending_child() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (node, _posted_txs) = MockNode::with_posted_channel();
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);
        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        let (tx, msg_id, inscription_sig) = sequencer
            .handle()
            .prepare_tx(Ops::new_unchecked(Vec::new()), b"prepared first".into())
            .unwrap();
        let signed_tx =
            SignedOps::from_parts(tx, OpProofs::from([OpProof::Ed25519Sig(inscription_sig)]))
                .expect("a valid inscription-only tx");

        sequencer
            .handle()
            .publish(b"published in between".into())
            .await
            .unwrap();

        let result = sequencer.handle().submit_signed_tx(signed_tx, msg_id);
        assert!(
            matches!(result, Err(Error::ChannelStateChanged(_))),
            "stale parent must be refused, got {result:?}"
        );
        assert_eq!(sequencer.state.as_ref().unwrap().pending_publish_count(), 1);
    }

    /// A gap block's deposit surfaces once, in order, even when the first
    /// backfill failed, the stream reconnected and the chain switched to
    /// another fork and back before the re-delivered event backfills the gap.
    #[tokio::test]
    #[expect(clippy::too_many_lines, reason = "Test function.")]
    async fn gap_deposits_surface_after_a_failed_backfill_and_a_fork_switch() {
        use std::{
            collections::HashMap,
            sync::{Arc, atomic::AtomicUsize},
        };

        use lb_core::{
            events::DepositNote,
            mantle::{
                ledger::NoteId,
                ops::{OpId as _, channel::deposit::Metadata},
            },
        };
        use lb_groth16::Fr;
        use lb_key_management_system_service::keys::ZkPublicKey;

        use crate::test_support::{deposit_event, inscribe_op};

        // G(0) <- B1(A) <- B2(Y, D2) <- B3(D3)   canonical in the end
        //             \- C(Z)                    canonical in between
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let pk = ZkPublicKey::from(Fr::from(7u64));
        let deposit = |n: u32| {
            let op = DepositOp {
                channel_id,
                inputs: Inputs::new([NoteId::from(Fr::from(n))]),
                metadata: Metadata::try_from(vec![u8::try_from(n).unwrap()]).unwrap(),
            };
            let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(op.clone())]);
            let note = DepositNote {
                note_id: NoteId::from(Fr::from(1000 + u64::from(n))),
                value: 50,
                pk,
            };
            let event = deposit_event(&tx, &op, 50, vec![note]);
            (op.op_id(), tx, event)
        };
        let a = inscribe_op(channel_id, MsgId::root(), b"a");
        let y = inscribe_op(channel_id, a.id(), b"y");
        let z = inscribe_op(channel_id, a.id(), b"z");
        let (y_id, z_id) = (y.id(), z.id());
        let (d2, d2_tx, d2_event) = deposit(2);
        let (d3, d3_tx, d3_event) = deposit(3);
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
            vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(y)]), d2_tx],
        );
        let c = api_block(
            9,
            1,
            2,
            vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(z)])],
        );
        let b3 = api_block(3, 2, 3, vec![d3_tx]);
        let node = MockNode {
            scripts: scripts(vec![
                StreamScript {
                    events: vec![live_event(&b1), live_event(&b3)],
                    then: StreamEnd::Hang,
                },
                StreamScript {
                    events: vec![live_event(&c), live_event(&b3)],
                    then: StreamEnd::Hang,
                },
            ]),
            blocks: vec![b2],
            block_fetch_failures: Arc::new(AtomicUsize::new(1)),
            events: HashMap::from([(header_id(2), d2_event), (header_id(3), d3_event)]),
            ..MockNode::default()
        };
        let config = SequencerConfig {
            reconnect_delay: std::time::Duration::from_millis(20),
            ..SequencerConfig::new(funding_config())
        };
        let mut sequencer =
            ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
        let msg_ids = |txs: &[ChannelUpdateTx]| {
            txs.iter()
                .filter_map(|tx| tx.inscription().map(|info| info.this_msg))
                .collect::<Vec<_>>()
        };

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut before = Vec::new();
        let (update, deposits) = loop {
            let event = tokio::time::timeout_at(deadline, sequencer.next_event())
                .await
                .expect("B3 lands after the reconnect and the fork switch");
            let Event::BlocksProcessed {
                channel_update,
                deposits,
                ..
            } = event
            else {
                continue;
            };
            if deposits.iter().any(|d| d.op_id == d2) {
                break (channel_update, deposits);
            }
            before.push(channel_update);
        };

        assert_eq!(sequencer.current_tip, Some(header_id(3)));
        assert!(
            before.iter().all(|u| !msg_ids(u.adopted()).contains(&y_id)),
            "Y is only adopted with the gap"
        );
        let switched_to_fork = before.last().expect("C was processed before B3");
        assert_eq!(msg_ids(switched_to_fork.adopted()), vec![z_id]);
        let observed: Vec<_> = deposits.iter().map(|d| d.op_id).collect();
        assert_eq!(
            observed,
            vec![d2, d3],
            "gap deposit first, then the live block's"
        );
        assert_eq!(msg_ids(update.adopted()), vec![y_id]);
        assert_eq!(msg_ids(update.orphaned()), vec![z_id]);
    }

    #[tokio::test]
    async fn cancelled_next_event_resumes_the_pulled_block() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let first_block = api_block(1, 0, 1, Vec::new());
        let second_block = api_block(2, 1, 2, Vec::new());
        let (gate_tx, gate_rx) = watch::channel(true);
        let (calls_tx, mut calls_rx) = mpsc::unbounded_channel();
        let node = MockNode {
            scripts: scripts(vec![StreamScript {
                events: vec![live_event(&first_block), live_event(&second_block)],
                then: StreamEnd::Hang,
            }]),
            channel_state_gate: Some(gate_rx),
            channel_state_calls: Some(calls_tx),
            ..MockNode::default()
        };
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        assert!(matches!(
            sequencer.next_event().await,
            Event::BlocksProcessed { .. }
        ));
        assert!(matches!(
            sequencer.next_event().await,
            Event::TurnNotification { .. }
        ));

        while calls_rx.try_recv().is_ok() {}
        gate_tx.send(false).unwrap();

        {
            let next_event = sequencer.next_event();
            tokio::pin!(next_event);

            tokio::select! {
                call = calls_rx.recv() => {
                    call.expect("channel-state call should be observed");
                }
                event = &mut next_event => {
                    panic!("block processing completed while its node request was gated: {event:?}");
                }
            }
        }

        assert_eq!(
            sequencer
                .pending_block_event
                .as_ref()
                .map(|event| event.block.header.id),
            Some(header_id(2))
        );

        gate_tx.send(true).unwrap();
        let resumed =
            tokio::time::timeout(std::time::Duration::from_secs(1), sequencer.next_event())
                .await
                .expect("the retained block should resume after cancellation");

        assert!(matches!(resumed, Event::BlocksProcessed { .. }));
        assert_eq!(sequencer.current_tip, Some(header_id(2)));
        assert!(sequencer.pending_block_event.is_none());

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), sequencer.next_event(),)
                .await
                .is_err(),
            "the resumed block must not be emitted twice"
        );
    }

    #[tokio::test]
    async fn cancelled_finalized_backfill_restarts_without_partial_state() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let first_block = api_block(1, 0, 1, Vec::new());
        let second_block = api_block(2, 1, 2, Vec::new());
        let second_event = ProcessedBlockEvent {
            block: second_block.clone(),
            tip: second_block.header.id,
            tip_slot: second_block.header.slot,
            lib: first_block.header.id,
            lib_slot: first_block.header.slot,
        };
        let (gate_tx, gate_rx) = watch::channel(true);
        let (calls_tx, mut calls_rx) = mpsc::unbounded_channel();
        let node = MockNode {
            scripts: scripts(vec![StreamScript {
                events: vec![live_event(&first_block), second_event],
                then: StreamEnd::Hang,
            }]),
            immutable: vec![first_block.clone()],
            immutable_blocks_gate: Some(gate_rx),
            immutable_blocks_calls: Some(calls_tx),
            ..MockNode::default()
        };
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        assert!(matches!(
            sequencer.next_event().await,
            Event::BlocksProcessed { .. }
        ));
        assert!(matches!(
            sequencer.next_event().await,
            Event::TurnNotification { .. }
        ));

        while calls_rx.try_recv().is_ok() {}
        gate_tx.send(false).unwrap();

        {
            let next_event = sequencer.next_event();
            tokio::pin!(next_event);

            tokio::select! {
                call = calls_rx.recv() => {
                    call.expect("immutable-blocks call should be observed");
                }
                event = &mut next_event => {
                    panic!("finalized backfill completed while its node request was gated: {event:?}");
                }
            }
        }

        assert_eq!(sequencer.lib_slot, Slot::genesis());
        assert_eq!(sequencer.current_tip, Some(first_block.header.id));
        assert_eq!(
            sequencer
                .pending_block_event
                .as_ref()
                .map(|event| event.block.header.id),
            Some(second_block.header.id)
        );

        gate_tx.send(true).unwrap();
        let resumed =
            tokio::time::timeout(std::time::Duration::from_secs(1), sequencer.next_event())
                .await
                .expect("the finalized backfill should resume after cancellation");

        assert!(matches!(resumed, Event::BlocksProcessed { .. }));
        assert_eq!(sequencer.lib_slot, first_block.header.slot);
        assert_eq!(sequencer.current_tip, Some(second_block.header.id));
        assert!(sequencer.pending_block_event.is_none());
    }

    /// A `SequencerClient::publish` issued while the node is down (reconnect
    /// in progress) must resolve promptly with [`Error::Unavailable`] —
    /// funding needs the node — instead of blocking until connectivity is
    /// restored. Once the node is back and `Ready` is re-announced,
    /// publishing works again.
    #[tokio::test]
    async fn client_publish_fails_fast_during_reconnect_and_recovers() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (up_tx, up_rx) = watch::channel(true);
        let (mut node, mut posted_txs) = MockNode::with_posted_channel();
        node.up = Some(up_rx);
        let config = SequencerConfig {
            reconnect_delay: std::time::Duration::from_millis(20),
            resubmit_interval: std::time::Duration::from_millis(20),
            ..SequencerConfig::new(funding_config())
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

        // After `Ready` on this single-key channel the turn-to-write watch is
        // true; the stream drop clears it, giving a deterministic signal that
        // the sequencer observed the disconnect before we publish.
        let mut turn_rx = client.subscribe_turn_to_write();
        assert!(
            turn_rx.borrow_and_update().our_turn_to_write,
            "single-key channel must report our turn after Ready"
        );

        // Take the node down: the live stream ends and the sequencer enters
        // reconnect (subsequent `block_stream` calls error).
        up_tx.send(false).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if !turn_rx.borrow_and_update().our_turn_to_write {
                    break;
                }
                tokio::select! {
                    changed = turn_rx.changed() => changed.unwrap(),
                    () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
                }
            }
        })
        .await
        .expect("stream drop must clear turn-to-write");

        // A client publish while the node is down must resolve promptly. We
        // drive `next_event` concurrently; the publish is serviced from inside
        // `wait_reconnect_delay` while the node is still down. With the old
        // behavior the request would never be drained during reconnect and this
        // would hang (caught by the timeout).
        let publish = client.publish(b"during-reconnect".into());
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = publish => result,
                () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
            }
        })
        .await
        .expect("client publish must resolve during reconnect, not block on connectivity");
        assert!(
            matches!(result, Err(Error::Unavailable { .. })),
            "publish while disconnected must fail fast, got {result:?}"
        );

        // Bring the node back up and wait for the re-announced `Ready`.
        up_tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if matches!(sequencer.next_event().await, Event::Ready) {
                    break;
                }
            }
        })
        .await
        .expect("Ready should be re-announced after reconnect");

        // Publishing works again and the inscription is posted.
        let publish = client.publish(b"after-reconnect".into());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = publish => result,
                () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
            }
        })
        .await
        .expect("client publish must resolve after reconnect")
        .expect("publish should succeed after reconnect");
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
                .op_refs()
                .into_iter()
                .any(|op| matches!(op, OpRef::ChannelInscribe(_))),
            "posted tx should carry the inscription published during reconnect"
        );
    }

    /// `TurnNotification` reaches `next_event` callers and the events
    /// broadcast once each, in the same order, after the block that changed
    /// the turn; the turn watch flips as soon as the change is detected.
    #[tokio::test]
    async fn next_event_yields_turn_notifications() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (up_tx, up_rx) = watch::channel(true);
        let (mut node, _posted_txs) = MockNode::with_posted_channel();
        node.up = Some(up_rx);
        let config = SequencerConfig {
            reconnect_delay: std::time::Duration::from_millis(20),
            resubmit_interval: std::time::Duration::from_millis(20),
            ..SequencerConfig::new(funding_config())
        };
        let mut sequencer =
            ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
        let mut events_rx = sequencer.subscribe_events();
        let mut turn_rx = sequencer.subscribe_turn_to_write();
        turn_rx.mark_unchanged();

        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        // The watch already reflects the turn before either channel delivers
        // the event.
        assert!(turn_rx.has_changed().unwrap());
        assert!(turn_rx.borrow_and_update().our_turn_to_write);

        // Single-key channel: the block that made us ready also made it our
        // turn. `BlocksProcessed` comes first, then the turn.
        let first = sequencer.next_event().await;
        assert!(
            matches!(first, Event::BlocksProcessed { .. }),
            "block event should precede the turn change, got {first:?}"
        );
        let second = sequencer.next_event().await;
        let Event::TurnNotification { notification } = second else {
            panic!("expected TurnNotification after the block event, got {second:?}");
        };
        assert!(notification.our_turn_to_write);

        // The broadcast carries the same events, once each, in the same order.
        let mut broadcast = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            broadcast.push(event);
        }
        let kinds: Vec<_> = broadcast
            .iter()
            .map(|event| match event {
                Event::Ready => "ready",
                Event::BlocksProcessed { .. } => "block",
                Event::TurnNotification { notification } if notification.our_turn_to_write => {
                    "our turn"
                }
                Event::TurnNotification { .. } => "not our turn",
                Event::MempoolPending(_) => "mempool",
            })
            .collect();
        assert_eq!(
            kinds,
            ["not our turn", "block", "ready", "block", "our turn"],
            "broadcast: {broadcast:?}"
        );

        // A stream drop clears the turn; that change is returned too, and
        // broadcast exactly once.
        up_tx.send(false).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Event::TurnNotification { notification } = sequencer.next_event().await
                    && !notification.our_turn_to_write
                {
                    break;
                }
            }
        })
        .await
        .expect("stream drop must yield a not-our-turn notification via next_event");
        let mut cleared = 0;
        while let Ok(event) = events_rx.try_recv() {
            if let Event::TurnNotification { notification } = event {
                assert!(!notification.our_turn_to_write);
                cleared += 1;
            }
        }
        assert_eq!(
            cleared, 1,
            "the cleared turn must be broadcast exactly once"
        );
    }

    /// The turn is re-evaluated on its own slot boundary: with no block after
    /// the first one, `next_event` still yields the alternating turn changes.
    #[tokio::test]
    async fn turn_notification_fires_on_timeframe_boundary_without_blocks() {
        assert_turns_alternate_without_blocks(1, 0).await;
    }

    /// Same for the timeout rotation, anchored at the last landed inscription.
    #[tokio::test]
    async fn turn_notification_fires_on_timeout_boundary_without_blocks() {
        assert_turns_alternate_without_blocks(0, 1).await;
    }

    async fn assert_turns_alternate_without_blocks(posting_timeframe: u32, posting_timeout: u32) {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let mut channel = single_key_channel_state();
        channel.accredited_keys = UnverifiedChannelKeys::try_from(vec![
            sequencer_key.public_key().into_unverified(),
            Ed25519Key::from_bytes(&[1; 32])
                .public_key()
                .into_unverified(),
        ])
        .unwrap()
        .into();
        channel.posting_timeframe = posting_timeframe.into();
        channel.posting_timeout = posting_timeout.into();
        let node = MockNode {
            channel_state: Some(channel),
            slot_duration_ms: 100,
            ..MockNode::default()
        };
        let config = SequencerConfig {
            resubmit_interval: std::time::Duration::from_secs(600),
            ..SequencerConfig::new(funding_config())
        };
        let mut sequencer =
            ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        let started = std::time::Instant::now();
        let mut turns = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while turns.len() < 4 {
                if let Event::TurnNotification { notification } = sequencer.next_event().await {
                    turns.push(notification.our_turn_to_write);
                }
            }
        })
        .await
        .expect("turn changes must arrive without blocks");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "turn changes should follow the 100ms slots, took {:?}",
            started.elapsed()
        );
        assert!(
            turns.windows(2).all(|pair| pair[0] != pair[1]),
            "a two-key channel rotating every slot alternates: {turns:?}"
        );
    }

    /// With a publish margin the turn watch closes before the rotation, at
    /// the same slot `can_publish_inscription_now` starts refusing.
    #[tokio::test]
    async fn turn_notification_closes_with_the_publish_margin() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let mut channel = single_key_channel_state();
        channel.posting_timeframe = 4u32.into();
        let node = MockNode {
            channel_state: Some(channel),
            slot_duration_ms: 300,
            ..MockNode::default()
        };
        let config = SequencerConfig {
            min_slots_remaining_in_turn: 3,
            resubmit_interval: std::time::Duration::from_secs(600),
            ..SequencerConfig::new(funding_config())
        };
        let mut sequencer =
            ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        // Single key: every turn is ours, open for the first two of its four
        // slots and closed for the last two. The notification at Ready is an
        // observation mid-turn; the first close is the first boundary.
        let mut seen = 0;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while seen < 4 {
                let Event::TurnNotification { notification } = sequencer.next_event().await else {
                    continue;
                };
                if seen == 0 && notification.our_turn_to_write {
                    continue;
                }
                assert_eq!(
                    notification.our_turn_to_write,
                    sequencer.can_publish_inscription_now(),
                    "watch and publish gate disagree: {notification:?}"
                );
                let current = notification.current_slot.unwrap();
                let expected = if notification.our_turn_to_write {
                    notification.starting_slot.unwrap()
                } else {
                    notification.ends_at_slot.unwrap() - 3 + 1
                };
                assert_eq!(
                    current, expected,
                    "flipped at the wrong slot: {notification:?}"
                );
                seen += 1;
            }
        })
        .await
        .expect("turn changes must arrive without blocks");
    }

    /// A `submit_signed_tx` bundle chains subsequent publishes off its last
    /// inscription — the config in it moves only the config lineage.
    /// Otherwise the next publish claims the same channel position as the
    /// bundle and the two race, permanently invalidating one side.
    #[tokio::test]
    async fn publish_after_bundle_chains_on_the_bundle_inscription_tip() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let node = MockNode::default();
        let mut sequencer = ZoneSequencer::init(
            channel_id,
            sequencer_key.clone(),
            node,
            funding_config(),
            None,
        );

        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        let inscribe = InscriptionOp {
            channel_id,
            inscription: b"bundle".to_vec().try_into().unwrap(),
            parent: MsgId::root(),
            signer: sequencer_key.public_key().into_unverified(),
        };
        let config = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: VerifiedChannelKeys::try_from(vec![sequencer_key.public_key()]).unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let inscribe_msg = inscribe.id();
        let bundle = unverified_tx_with_ops(vec![
            Op::ChannelInscribe(inscribe),
            Op::ChannelConfig(config),
        ]);

        let (result, _cp) = sequencer
            .handle()
            .submit_signed_tx(bundle, inscribe_msg)
            .expect("bundle submit should be accepted");
        assert_eq!(
            result.tx.inscription().this_msg,
            inscribe_msg,
            "the bundle's resulting tip is its inscription"
        );

        let (published, _cp) = sequencer
            .handle()
            .publish(b"after-bundle".into())
            .await
            .expect("publish after bundle should be accepted");
        assert_eq!(
            published.tx.inscription().parent_msg,
            inscribe_msg,
            "the next publish must chain after the pending bundle's inscription"
        );
    }

    #[tokio::test]
    async fn config_only_block_orphans_pending_inscription_but_keeps_message_tip() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

        let config_op = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: VerifiedChannelKeys::try_from(vec![
                Ed25519Key::from_bytes(&[0; 32]).public_key(),
            ])
            .unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let config_tx = unverified_tx_with_ops(vec![Op::ChannelConfig(config_op)]);
        let config_hash = config_tx.hash();
        let config_block = api_block(2, 1, 2, vec![config_tx]);

        // Second connection (the config block) is gated behind `up` so it
        // cannot be consumed before the publish is in.
        let (up_tx, up_rx) = watch::channel(true);
        let node = MockNode {
            up: Some(up_rx),
            scripts: scripts(vec![
                StreamScript {
                    events: vec![live_event(&api_block(1, 0, 1, Vec::new()))],
                    then: StreamEnd::Hang,
                },
                StreamScript {
                    events: vec![live_event(&config_block)],
                    then: StreamEnd::Hang,
                },
            ]),
            ..MockNode::default()
        };
        let config = SequencerConfig {
            reconnect_delay: std::time::Duration::from_millis(20),
            resubmit_interval: std::time::Duration::from_millis(20),
            ..SequencerConfig::new(funding_config())
        };
        let mut sequencer =
            ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
        let client = sequencer.client();

        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }

        let mut status_rx = client.subscribe_tx_status();
        let publish = client.publish(b"survives-config".into());
        let (result, _checkpoint) =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::select! {
                    result = publish => result,
                    () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
                }
            })
            .await
            .expect("publish must resolve")
            .expect("publish should be accepted after Ready");
        let p_hash = result.inscription_id();

        // Keep driving between the toggles so the down-edge is observed. The
        // config block is recognized by the `OnChain` status of its tx; the
        // `BlocksProcessed` that follows it carries the state to assert on.
        up_tx.send(false).unwrap();
        let (checkpoint, update) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let toggle = async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                up_tx.send(true).unwrap();
            };
            let drive = async {
                let mut config_on_chain = false;
                loop {
                    let event = sequencer.next_event().await;
                    while let Ok(update) = status_rx.try_recv() {
                        config_on_chain |= update.tx_hash == config_hash
                            && matches!(update.status, TxStatus::OnChain(_));
                    }
                    if config_on_chain
                        && let Event::BlocksProcessed {
                            checkpoint,
                            channel_update,
                            ..
                        } = event
                    {
                        return (checkpoint, channel_update);
                    }
                }
            };
            let ((), out) = tokio::join!(toggle, drive);
            out
        })
        .await
        .expect("the config-only block must be processed");

        // The config changes the channel view, so the pending inscription is
        // shed and reported orphaned (to be resubmitted against the new view).
        assert!(update.orphaned().iter().any(|tx| tx.tx_hash() == p_hash));
        assert!(checkpoint.pending_txs.iter().all(|(h, _)| *h != p_hash));
        assert!(
            update
                .canonical_chain()
                .is_some_and(|mut c| c.all(|tx| tx.tx_hash() != p_hash))
        );
        // The chaining pointer resets to the (unchanged) message tip so the
        // resubmit re-posts there. Nothing was mined, so the tip is root.
        assert_eq!(
            checkpoint.last_msg_id,
            MsgId::root(),
            "the pointer resets to the message tip (root)"
        );
    }

    #[test]
    fn track_pending_tx_classifies_atomic_bundle_with_withdraws() {
        // Bundle: [ChannelWithdraw(channel_id), ChannelInscribe(channel_id)]
        // Restore should put it in pending (not pending_other) with the
        // withdraws field populated, so on orphan we emit
        // ChannelUpdateTx::AtomicWithdraw (not Inscription).
        use lb_core::mantle::NoteId;
        use lb_groth16::Fr;

        let channel_id = ChannelId::from([1u8; 32]);
        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([NoteId::from(Fr::from(0u64))]),
        };
        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32])
                .public_key()
                .into_unverified(),
        };
        let mantle_tx = Ops::from([
            Op::ChannelWithdraw(withdraw_op.clone()),
            Op::ChannelInscribe(inscribe_op),
        ]);
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedOps::from_ops_with_sample_proofs(mantle_tx);

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        track_pending_tx(&mut state, signed_tx, channel_id).unwrap();

        let pending = state
            .pending_inscription(&tx_hash)
            .expect("bundle should be in pending inscriptions");
        let PendingBundle::Withdraw { withdraws, .. } = &pending.bundle else {
            panic!("bundle should be a withdraw bundle");
        };
        assert_eq!(withdraws.len(), 1, "bundle should carry one WithdrawInfo");
        assert_eq!(withdraws[0].op, withdraw_op);
        assert!(
            !state.pending_other_contains(&tx_hash),
            "bundle should not be in pending_other"
        );
    }

    #[test]
    fn track_pending_tx_classifies_plain_inscription_with_none_withdraws() {
        // Plain inscription: pending with `withdraws == None`.
        let channel_id = ChannelId::from([2u8; 32]);
        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32])
                .public_key()
                .into_unverified(),
        };
        let mantle_tx = Ops::from([Op::ChannelInscribe(inscribe_op)]);
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedOps::from_ops_with_sample_proofs(mantle_tx);

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        track_pending_tx(&mut state, signed_tx, channel_id).unwrap();

        let pending = state
            .pending_inscription(&tx_hash)
            .expect("plain inscription should be in pending inscriptions");
        assert!(matches!(pending.bundle, PendingBundle::Plain));
    }

    #[test]
    fn track_pending_tx_falls_back_to_other_when_no_inscribe_for_channel() {
        // Inscribe for a different channel: should fall back to pending_other
        // (treated as opaque).
        let our_channel = ChannelId::from([3u8; 32]);
        let other_channel = ChannelId::from([4u8; 32]);
        let inscribe_op = InscriptionOp {
            channel_id: other_channel,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32])
                .public_key()
                .into_unverified(),
        };
        let mantle_tx = Ops::from([Op::ChannelInscribe(inscribe_op)]);
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedOps::from_ops_with_sample_proofs(mantle_tx);

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        track_pending_tx(&mut state, signed_tx, our_channel).unwrap();

        assert!(
            state.pending_inscription(&tx_hash).is_none(),
            "wrong-channel tx should not be in pending inscriptions"
        );
        assert!(
            state.pending_other_contains(&tx_hash),
            "wrong-channel tx should be in pending_other"
        );
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
            signer: sequencer_key.public_key().into_unverified(),
        };
        let expected_msg_id = inscribe.id();
        let genesis_tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(inscribe)]);
        let genesis_tx_hash = genesis_tx.hash();

        let genesis_block = api_block(1, 0, 0, vec![genesis_tx]);
        // Empty block at slot 1 so the block stream advances and the
        // sequencer signals `Ready`, giving the test a clean exit signal.
        let live_block = api_block(2, 1, 1, Vec::new());

        let node = MockNode {
            lib: genesis_block.header.id,
            tip: genesis_block.header.id,
            scripts: scripts(vec![StreamScript {
                events: vec![ProcessedBlockEvent {
                    block: live_block.clone(),
                    tip: live_block.header.id,
                    tip_slot: live_block.header.slot,
                    lib: genesis_block.header.id,
                    lib_slot: Slot::genesis(),
                }],
                then: StreamEnd::Hang,
            }]),
            immutable: vec![genesis_block],
            ..MockNode::default()
        };
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

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

    /// The finalized config tip must survive the real persistence path —
    /// `build_checkpoint` → serde → `init_with_config` — not just the in-state
    /// setter. A config-free resume must leave it intact, surfaced through the
    /// checkpoint the sequencer re-emits.
    #[tokio::test]
    async fn finalized_config_survives_checkpoint_persistence_and_resume() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let finalized_config = MsgId::from([9; 32]);

        // `build_checkpoint` captures the finalized config tip.
        let mut state = TxState::new(header_id(0), MsgId::root());
        state.set_finalized_config(finalized_config);
        let cp = build_checkpoint(&state, MsgId::root(), Slot::genesis());
        assert_eq!(
            cp.finalized_config, finalized_config,
            "build_checkpoint saves it"
        );

        // It survives serde (and `serde(default)` does not clobber a set value).
        let json = serde_json::to_string(&cp).expect("serialize checkpoint");
        let cp: SequencerCheckpoint = serde_json::from_str(&json).expect("deserialize checkpoint");
        assert_eq!(
            cp.finalized_config, finalized_config,
            "serde round-trips it"
        );

        // `init_with_config` restores it; a config-free resume keeps it.
        let genesis_block = api_block(0, 0, 0, Vec::new());
        let live_block = api_block(1, 0, 1, Vec::new());
        let node = MockNode {
            lib: header_id(0),
            tip: header_id(0),
            immutable: vec![genesis_block],
            scripts: scripts(vec![StreamScript {
                events: vec![live_event(&live_block)],
                then: StreamEnd::Hang,
            }]),
            ..MockNode::default()
        };
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), Some(cp));

        let restored = loop {
            match sequencer.next_event().await {
                Event::BlocksProcessed { checkpoint, .. } => {
                    break checkpoint.finalized_config;
                }
                Event::Ready | Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
            }
        };
        assert_eq!(
            restored, finalized_config,
            "resume restores finalized_config through the checkpoint"
        );
    }

    /// A config finalized during downtime, replayed by the resume backfill,
    /// must refine `finalized_config` past the (stale) checkpoint seed —
    /// exercising the backfill's config arm, not just the seed.
    #[tokio::test]
    async fn resume_backfill_refines_finalized_config_from_a_replayed_config() {
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

        // A pure config sits in the finalized history the backfill replays.
        let config_op = ChannelConfigOp {
            channel: channel_id,
            parent: MsgId::root(),
            keys: VerifiedChannelKeys::try_from(vec![
                Ed25519Key::from_bytes(&[0; 32]).public_key(),
            ])
            .unwrap(),
            posting_timeframe: SlotTimeframe::from(0u32),
            posting_timeout: SlotTimeout::from(0u32),
            configuration_threshold: 1,
            transfer_threshold: 1,
        };
        let config_id = config_op.id();
        let config_tx = unverified_tx_with_ops(vec![Op::ChannelConfig(config_op)]);
        // The config finalized during downtime at slot 1 — above the checkpoint
        // LIB (genesis), so the resume backfill must replay it.
        let config_block = api_block(1, 0, 1, vec![config_tx]);

        // The checkpoint's finalized_config is stale; the backfill must move it.
        let stale = MsgId::from([1; 32]);
        let mut state = TxState::new(header_id(0), MsgId::root());
        state.set_finalized_config(stale);
        let cp = build_checkpoint(&state, MsgId::root(), Slot::genesis());

        // A live block at slot 2 whose LIB is the config block (slot 1), so the
        // backfill catches up [genesis..=slot 1] and replays the config.
        let live_block = api_block(2, 1, 2, Vec::new());
        let event = ProcessedBlockEvent {
            block: live_block.clone(),
            tip: live_block.header.id,
            tip_slot: live_block.header.slot,
            lib: header_id(1),
            lib_slot: Slot::from(1),
        };
        let node = MockNode {
            lib: header_id(1),
            tip: header_id(1),
            immutable: vec![config_block],
            scripts: scripts(vec![StreamScript {
                events: vec![event],
                then: StreamEnd::Hang,
            }]),
            ..MockNode::default()
        };
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), Some(cp));

        let restored = loop {
            match sequencer.next_event().await {
                Event::BlocksProcessed { checkpoint, .. } => {
                    break checkpoint.finalized_config;
                }
                Event::Ready | Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
            }
        };
        assert_eq!(
            restored, config_id,
            "backfill refines finalized_config to the replayed config"
        );
    }

    /// Realistic stream-gap scenario, driven end-to-end through the public
    /// `ZoneSequencer` event loop.
    ///
    /// Chain: G <- B1 <- B2 <- B3, one canonical branch, LIB at genesis.
    /// The live stream delivers B1 (inscription A) and drops. B2 (inscription
    /// Y, child of A) is mined during the outage. The stream resumes at B3;
    /// the sequencer self-heals by backfilling B2.
    ///
    /// Per the [`Event::Ready`] / [`ChannelUpdate`] contract, catch-up deltas
    /// surface on the next `BlocksProcessed` once the stream resumes — so Y
    /// must be reported as `adopted`. A consumer mirroring the channel from
    /// `ChannelUpdate` otherwise silently misses Y until finalization.
    #[tokio::test]
    async fn stream_gap_surfaces_backfilled_inscriptions_as_adopted() {
        use std::time::Duration;

        use tokio::time::timeout;

        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

        let a = InscriptionOp {
            channel_id,
            parent: MsgId::root(),
            inscription: Inscription::new_unchecked(b"a".to_vec()),
            signer: sequencer_key.public_key().into_unverified(),
        };
        let a_id = a.id();
        let y = InscriptionOp {
            channel_id,
            parent: a_id,
            inscription: Inscription::new_unchecked(b"y".to_vec()),
            signer: sequencer_key.public_key().into_unverified(),
        };
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
            scripts: scripts(vec![
                StreamScript {
                    events: vec![live_event(&b1)],
                    then: StreamEnd::End,
                },
                StreamScript {
                    events: vec![live_event(&b3)],
                    then: StreamEnd::Hang,
                },
            ]),
            blocks: vec![b2],
            ..MockNode::default()
        };
        let mut sequencer =
            ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

        // Phase 1: drive until B1's channel update adopts A (Ready and turn
        // notifications interleave).
        let adopted = timeout(Duration::from_secs(10), async {
            loop {
                if let Event::BlocksProcessed { channel_update, .. } = sequencer.next_event().await
                    && !channel_update.adopted().is_empty()
                {
                    return channel_update.adopted().to_vec();
                }
            }
        })
        .await
        .expect("timed out waiting for B1's channel update");
        assert!(
            adopted
                .iter()
                .any(|t| t.inscription().is_some_and(|i| i.this_msg == a_id)),
            "sanity: A is adopted from the live B1"
        );

        // Phase 2: stream #1 has ended — a disconnect. `next_event`
        // reconnects internally and resumes at B3; the canonical backfill
        // fetches the missed B2. The first `BlocksProcessed` after the
        // reconnect is B3's ingestion and must carry Y as adopted.
        let update = timeout(Duration::from_secs(10), async {
            loop {
                if let Event::BlocksProcessed { channel_update, .. } = sequencer.next_event().await
                {
                    return channel_update;
                }
            }
        })
        .await
        .expect("timed out waiting for the post-reconnect BlocksProcessed");
        assert!(
            update
                .adopted()
                .iter()
                .any(|t| t.inscription().is_some_and(|i| i.this_msg == y_id)),
            "inscription mined during the stream gap must surface as adopted on the next \
             BlocksProcessed after reconnect; got {update:?}",
        );
    }

    /// The variant follows `orphaned`: nothing orphaned is an extension,
    /// otherwise a conflict whose prefix excludes the orphaned entries.
    #[tokio::test]
    async fn update_variant_follows_orphaned() {
        let channel_id = ChannelId::from([0; 32]);
        let key = Ed25519Key::from_bytes(&[0; 32]);
        let mut sequencer = ready_sequencer_with_channel(None, key.clone()).await;
        let entry = |n: u8| {
            let op = InscriptionOp {
                channel_id,
                inscription: Inscription::new_unchecked(vec![n]),
                parent: MsgId::root(),
                signer: key.public_key().into_unverified(),
            };
            let tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(op.clone())]);
            ChannelUpdateTx::Inscription(InscriptionInfo {
                tx_hash: tx.hash(),
                parent_msg: MsgId::root(),
                this_msg: MsgId::root(),
                payload: op.inscription,
                signer: Some(op.signer),
            })
        };
        let result =
            |adopted: Vec<ChannelUpdateTx>, orphaned: Vec<ChannelUpdateTx>| BlockEventResult {
                finalized_items: Vec::new(),
                channel_update: Some(ChannelUpdateInfo {
                    orphaned,
                    adopted,
                    new_channel_tip: MsgId::root(),
                }),
                common_prefix: vec![entry(1), entry(2)],
                mined_inscriptions: Vec::new(),
                deposits: Vec::new(),
            };

        let (update, ..) = sequencer.apply_block_result(result(vec![entry(3)], Vec::new()));
        assert!(matches!(update, ChannelUpdate::Extension { adopted } if adopted.len() == 1));

        let (update, ..) = sequencer.apply_block_result(result(vec![entry(3)], vec![entry(2)]));
        let ChannelUpdate::Conflict {
            common_prefix,
            adopted,
            orphaned,
        } = update
        else {
            panic!("an orphaned entry makes a conflict")
        };
        let hashes =
            |txs: &[ChannelUpdateTx]| txs.iter().map(ChannelUpdateTx::tx_hash).collect::<Vec<_>>();
        assert_eq!(hashes(&common_prefix), hashes(&[entry(1)]));
        assert_eq!(hashes(&adopted), hashes(&[entry(3)]));
        assert_eq!(hashes(&orphaned), hashes(&[entry(2)]));
    }

    async fn ready_sequencer_with_channel(
        channel: Option<ChannelState>,
        sequencer_key: Ed25519Key,
    ) -> ZoneSequencer<MockNode> {
        let (mut node, _posted_txs) = MockNode::with_posted_channel();
        node.channel_state = channel;
        let mut sequencer = ZoneSequencer::init(
            ChannelId::from([0; 32]),
            sequencer_key,
            node,
            funding_config(),
            None,
        );
        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }
        sequencer
    }

    /// The config op's signature must claim our key's index in the *current*
    /// accredited list — not index 0 — or the ledger rejects the update as
    /// `InvalidSignature` whenever the sequencer is not the leading key.
    #[tokio::test]
    async fn channel_config_signs_with_own_current_accredited_index() {
        let own_key = Ed25519Key::from_bytes(&[7; 32]);
        let leading_key = Ed25519Key::from_bytes(&[0; 32]);
        let channel = ChannelState {
            accredited_keys: UnverifiedChannelKeys::new_unchecked(vec![
                leading_key.public_key().into_unverified(),
                own_key.public_key().into_unverified(),
            ])
            .into(),
            ..single_key_channel_state()
        };
        let mut sequencer = ready_sequencer_with_channel(Some(channel), own_key.clone()).await;

        let (_receipt, signed_ops) = sequencer
            .handle()
            .channel_config(
                VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
                SlotTimeframe::from(0u32),
                SlotTimeout::from(0u32),
                1,
                1,
            )
            .await
            .expect("config update from an accredited non-leading key must build");

        assert_eq!(
            config_op_of(&signed_ops).parent,
            MsgId::root(),
            "parent must equal the channel's config tip"
        );
        let OpProofRef::ChannelMultiSigProof(proof) =
            signed_ops.first().expect("config op proof present").proof()
        else {
            panic!("config op must carry a multi-sig proof");
        };
        let signatures = proof.signatures();
        assert_eq!(signatures.len(), 1);
        assert_eq!(
            signatures[0].channel_key_index, 1,
            "signature must claim the signer's position in the current accredited list"
        );
        own_key
            .public_key()
            .into_unverified()
            .verify(
                signed_ops.hash().as_signing_bytes(),
                &signatures[0].signature,
            )
            .expect("signature must verify against the claimed key over the funded tx hash");
    }

    /// Configuring an unclaimed channel requires no signatures (the ledger
    /// skips the check), so the proof must stay empty — a superfluous
    /// signature would also break the node wallet's fee prediction.
    #[tokio::test]
    async fn channel_config_on_unclaimed_channel_carries_empty_proof() {
        let own_key = Ed25519Key::from_bytes(&[7; 32]);
        let mut sequencer = ready_sequencer_with_channel(None, own_key.clone()).await;

        let (_receipt, signed_ops) = sequencer
            .handle()
            .channel_config(
                VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
                SlotTimeframe::from(0u32),
                SlotTimeout::from(0u32),
                1,
                1,
            )
            .await
            .expect("claiming an unclaimed channel must build");

        let OpProofRef::ChannelMultiSigProof(proof) =
            signed_ops.first().expect("config op proof present").proof()
        else {
            panic!("config op must carry a multi-sig proof");
        };
        assert!(proof.signatures().is_empty());
        assert_eq!(
            config_op_of(&signed_ops).parent,
            MsgId::root(),
            "claiming an unclaimed channel must be rooted at ZERO"
        );
    }

    /// A sequencer whose key is not on the current accredited list cannot
    /// produce a verifiable config signature; fail locally instead of
    /// submitting a transaction that silently dies at block assembly.
    #[tokio::test]
    async fn channel_config_fails_when_not_accredited() {
        let own_key = Ed25519Key::from_bytes(&[7; 32]);
        let mut sequencer =
            ready_sequencer_with_channel(Some(single_key_channel_state()), own_key.clone()).await;

        let error = sequencer
            .handle()
            .channel_config(
                VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
                SlotTimeframe::from(0u32),
                SlotTimeout::from(0u32),
                1,
                1,
            )
            .await
            .expect_err("config update from a non-accredited key must fail locally");
        assert!(
            error.to_string().contains("accredited"),
            "unexpected error: {error}"
        );
    }

    /// `configuration_threshold > 1` needs signatures the sequencer cannot
    /// collect; reject early with a clear error.
    #[tokio::test]
    async fn channel_config_rejects_multi_sig_threshold() {
        let own_key = Ed25519Key::from_bytes(&[7; 32]);
        let channel = ChannelState {
            accredited_keys: UnverifiedChannelKeys::new_unchecked(vec![
                own_key.public_key().into_unverified(),
                Ed25519Key::from_bytes(&[0; 32])
                    .public_key()
                    .into_unverified(),
            ])
            .into(),
            configuration_threshold: 2,
            ..single_key_channel_state()
        };
        let mut sequencer = ready_sequencer_with_channel(Some(channel), own_key.clone()).await;

        let error = sequencer
            .handle()
            .channel_config(
                VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
                SlotTimeframe::from(0u32),
                SlotTimeout::from(0u32),
                1,
                1,
            )
            .await
            .expect_err("multi-sig threshold config update must fail locally");
        assert!(
            error.to_string().contains("single-signer"),
            "unexpected error: {error}"
        );
    }

    fn config_op_of(tx: &SignedOps<Unverified, StandardMode>) -> ChannelConfigOp {
        tx.op_refs()
            .iter()
            .find_map(|op| match *op {
                OpRef::ChannelConfig(config) => Some(config.clone()),
                _ => None,
            })
            .expect("tx should carry a config op")
    }

    /// A configuration extends the mined config tip, never a config of ours
    /// still in flight: the proof is built for the mined key set, so pairing
    /// it with a pending config's id would produce a tx the ledger can only
    /// reject. Two configs issued back to back contest the same slot and the
    /// loser is shed once the winner lands.
    #[tokio::test]
    async fn consecutive_channel_configs_claim_the_mined_config_tip() {
        let own_key = Ed25519Key::from_bytes(&[7; 32]);
        let mut sequencer = ready_sequencer_with_channel(None, own_key.clone()).await;

        let (_receipt, first_tx) = sequencer
            .handle()
            .channel_config(
                VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
                SlotTimeframe::from(0u32),
                SlotTimeout::from(0u32),
                1,
                1,
            )
            .await
            .expect("first config should be accepted");
        let (_receipt, second_tx) = sequencer
            .handle()
            .channel_config(
                VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
                SlotTimeframe::from(1u32),
                SlotTimeout::from(0u32),
                1,
                1,
            )
            .await
            .expect("second config should be accepted");

        let first = config_op_of(&first_tx);
        let second = config_op_of(&second_tx);
        assert_eq!(
            first.parent,
            MsgId::root(),
            "the config claiming an unclaimed channel must be rooted at ZERO"
        );
        assert_eq!(
            second.parent, first.parent,
            "a config in flight must not become the parent of the next one"
        );
    }
}
