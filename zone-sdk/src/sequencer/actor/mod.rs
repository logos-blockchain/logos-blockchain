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
use tracing::{debug, error, info, warn};

use super::{
    TARGET,
    block_fetch::{BlockEventResult, classify_shed_other, handle_block_event, orphan_from_shed},
    slot_clock::{SlotClock, slot_to_u64},
    state::{ChannelUpdateInfo, RefundCandidate, TxState},
    tx_builder::{fund_builder, sign_own_tx},
    types::{
        ChannelUpdate, ChannelUpdateTx, DepositInfo, Error, Event, FinalizedTx,
        SequencerChannelView, SequencerCheckpoint, TurnNotification,
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
        let (channel_update, deposits, finalized) = self.apply_block_result(result);

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

    /// Rebuild own pending txs that stayed unmined past
    /// `stale_refund_slots`: re-fund the stored channel ops and re-sign, so
    /// the entry keeps its message id under a new tx hash and the following
    /// `resubmit_pending` posts it. Runs only when a post could follow, on
    /// our turn with a connected node: a rebuild that then waits for the
    /// turn would only age its fresh fee note. A rebuild of a merely slow tx
    /// is harmless: both claim the same lineage slot, so whichever lands
    /// first kills the other.
    pub(super) async fn refund_stale_pending(&mut self) {
        let window = self.config.stale_refund_slots;
        if window == 0 || !self.connected || !self.can_publish_inscription_now() {
            return;
        }
        let (Some(state), Some(tip)) = (self.state.as_ref(), self.current_tip) else {
            return;
        };
        let candidates = state.refund_candidates(tip, self.lib_slot, window);
        let mut changed = false;
        for candidate in candidates {
            if self.posting.contains(&candidate.tx_hash) {
                continue;
            }
            let old_hash = candidate.tx_hash;
            match self.refund_one(candidate).await {
                Ok(new_hash) => info!(
                    target: TARGET,
                    "Re-funded stale tx {} -> {}",
                    hex::encode(old_hash.0),
                    hex::encode(new_hash.0)
                ),
                Err(e) => {
                    warn!(
                        target: TARGET,
                        "Failed to re-fund stale tx {}: {e}; retrying after another window",
                        hex::encode(old_hash.0)
                    );
                    if let Some(state) = self.state.as_mut() {
                        state.stamp_funding(&old_hash, self.lib_slot, None);
                    }
                }
            }
            changed = true;
        }
        if changed {
            self.publish_checkpoint();
        }
    }

    /// Re-fund, re-sign and swap in one candidate; returns its new hash.
    async fn refund_one(&mut self, candidate: RefundCandidate) -> Result<TxHash, Error> {
        let own_key_index = match (candidate.bundle, self.own_key_index) {
            (false, _) => None,
            (true, Some(index)) => Some(index),
            (true, None) => {
                return Err(Error::Network(
                    "not on the accredited list; cannot re-sign the bundle".into(),
                ));
            }
        };
        let (tx, transfer_proof) =
            fund_builder(&self.node, &self.config.funding, candidate.pre_fund.clone()).await?;
        let signed = sign_own_tx(tx, transfer_proof, &self.signing_key, own_key_index)?;
        let funded_at = self.lib_slot;
        self.state
            .as_mut()
            .and_then(|state| {
                state.replace_pending(&candidate.tx_hash, signed, candidate.pre_fund, funded_at)
            })
            .ok_or_else(|| Error::Network("no longer pending".into()))
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
    ) -> (ChannelUpdate, Vec<DepositInfo>, Vec<FinalizedTx>) {
        let BlockEventResult {
            finalized_items,
            channel_update,
            common_prefix,
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

        self.shed_expired_into(&mut orphaned, &mut seen);

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

        (channel_update, deposits, finalized_items)
    }

    /// Shed into `orphaned` what is not mined on the current branch, was
    /// funded longer ago than the refund window and cannot be rebuilt here;
    /// own entries with pre-funding ops wait for the resubmit tick instead.
    fn shed_expired_into(
        &mut self,
        orphaned: &mut Vec<ChannelUpdateTx>,
        seen: &mut HashSet<TxHash>,
    ) {
        let (Some(state), Some(tip)) = (self.state.as_mut(), self.current_tip) else {
            return;
        };
        let (expired, expired_other) =
            state.shed_expired(tip, self.lib_slot, self.config.stale_refund_slots);
        if expired.is_empty() && expired_other.is_empty() {
            return;
        }
        self.last_msg_id = state.publish_parent(tip);
        let shed = expired.into_iter().map(orphan_from_shed).chain(
            expired_other
                .into_iter()
                .map(|tx| classify_shed_other(tx, self.channel_id)),
        );
        for tx in shed {
            if seen.insert(tx.tx_hash()) {
                warn!(target: TARGET, "Pending tx {} expired unmined; orphaned", hex::encode(tx.tx_hash().0));
                orphaned.push(tx);
            }
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
mod tests;
