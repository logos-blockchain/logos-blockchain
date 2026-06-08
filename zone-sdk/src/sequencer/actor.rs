#![allow(
    clippy::multiple_inherent_impl,
    reason = "`ZoneSequencer`'s public API lives in zone_sequencer.rs; internal handlers live here."
)]

use std::collections::VecDeque;

use lb_common_http_client::{ProcessedBlockEvent, Slot};
use lb_core::mantle::{
    MantleTx, SignedMantleTx, Transaction as _,
    channel::ChannelState,
    encoding::Ops,
    ops::{
        Op,
        channel::{
            MsgId,
            inscribe::{Inscription, InscriptionOp},
            withdraw::ChannelWithdrawOp,
        },
    },
};
use tracing::{debug, error, warn};

use super::{
    TARGET,
    block_fetch::{BlockEventResult, handle_block_event, orphan_from_shed},
    slot_clock::{SlotClock, slot_to_u64},
    state::{ChannelUpdateInfo, TxState},
    tx_builder::{
        build_atomic_withdraw_ops_proofs, create_channel_config_tx, create_inscribe_tx,
        find_own_key_index, prepare_tx, sign_tx,
    },
    types::{
        AtomicWithdrawInfo, Error, Event, InscriptionId, InscriptionInfo, PublishResult,
        PublishedTx, SequencerChannelView, SequencerCheckpoint, TurnNotification, WithdrawArg,
        WithdrawInfo,
    },
    zone_sequencer::{ActorRequest, InFlight, ZoneSequencer, build_checkpoint},
};
use crate::adapter;

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Handle a single item from the blocks stream. `None` means the stream
    /// disconnected; any other value is processed as a block event.
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    pub(super) async fn handle_stream_item(
        &mut self,
        maybe_event: Option<ProcessedBlockEvent>,
    ) -> Option<Event> {
        let Some(block_event) = maybe_event else {
            warn!(target: TARGET, "Blocks stream disconnected, will reconnect on next call");
            self.blocks_stream = None;
            return self.signal_not_ready();
        };

        let result = match handle_block_event(
            &block_event,
            &mut self.state,
            &mut self.current_tip,
            &mut self.lib_slot,
            self.channel_id,
            &self.node,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                error!(
                    target: TARGET,
                    "Block event processing failed; dropping stream so reconnect retries: {e}"
                );
                self.blocks_stream = None;
                return self.signal_not_ready();
            }
        };

        if let Some(slot_clock) = self.slot_clock.as_mut() {
            slot_clock.observe_slot(block_event.tip_slot);
        }

        if let Err(err) = self.refresh_channel_state().await {
            error!(
                target: TARGET,
                "Failed to refresh channel state after block; dropping stream so reconnect retries: {err}"
            );
            self.blocks_stream = None;
            return self.signal_not_ready();
        }

        let became_ready = self.maybe_signal_ready();
        let mut events = self.apply_block_result(result);

        self.enqueue_pending_submit();

        if let Some(checkpoint) = self.publish_checkpoint() {
            events.push_back(Event::Checkpoint { checkpoint });
        }

        if became_ready {
            // Preserve the existing public event contract: when readiness transitions,
            // Ready is emitted first. Any block-derived events and any Published event
            // produced by queue draining are buffered and emitted on subsequent
            // next_event() calls.
            self.buffered_events.extend(events);
            return Some(self.emit_now(Event::Readiness { ready: true }));
        }

        let event = events.pop_front()?;
        self.buffered_events.extend(events);

        Some(event)
    }

    /// If not yet ready and startup backfill is complete, mark ready. Returns
    /// true if readiness transitioned.
    fn maybe_signal_ready(&self) -> bool {
        if self.is_ready() {
            return false;
        }

        if self.backfill_from.is_none() && self.backfill_to.is_none() {
            debug!(target: TARGET, "Sequencer ready (backfill complete, first block processed)");
            let _ = self.ready_tx.send(true);
            true
        } else {
            debug!(target: TARGET,
                "Not yet ready: backfill_from={:?}, backfill_to={:?}",
                self.backfill_from, self.backfill_to
            );
            false
        }
    }

    /// Flip readiness to `false` and, if that was an actual transition (was
    /// previously `true`), surface [`Event::Readiness`] so the consumer's
    /// drive loop learns about the disconnect on the event stream. Returns
    /// `None` when readiness was already `false` (no spurious event).
    fn signal_not_ready(&self) -> Option<Event> {
        let mut transitioned = false;
        self.ready_tx.send_if_modified(|current| {
            if *current {
                *current = false;
                transitioned = true;
                true
            } else {
                false
            }
        });
        self.publish_turn_to_write(false);
        transitioned.then(|| self.emit_now(Event::Readiness { ready: false }))
    }

    /// Build the current checkpoint from internal state and publish it to the
    /// `checkpoint_tx` watch channel. Returns the built checkpoint (or `None`
    /// if state isn't initialised yet) so callers can reuse it to construct
    /// the matching [`Event::Checkpoint`].
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

    pub(super) fn publish_channel_view(&self) {
        let view = self.channel_view();
        let turn_to_write = self.is_ready() && view.our_turn_to_write;
        // `send_replace` so the stored value stays current even with no
        // subscribers (sync reads happen via late `subscribe_channel_view`).
        self.channel_view_tx.send_replace(view);
        self.publish_turn_to_write(turn_to_write);
    }

    fn publish_turn_to_write(&self, turn_to_write: bool) {
        let mut emitted: Option<TurnNotification> = None;

        self.turn_to_write_tx.send_if_modified(|current| {
            let new = self.turn_notification(turn_to_write);
            let changed = current.our_turn_to_write != new.our_turn_to_write
                || current.starting_slot != new.starting_slot
                || current.ends_at_slot != new.ends_at_slot
                || current.turn_to_write_slots != new.turn_to_write_slots;

            *current = new.clone();
            if changed {
                emitted = Some(new);
            }

            changed
        });

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

    fn can_publish_inscription_now(&self) -> bool {
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

    /// Enqueue pending signed txs for posting. Channel inscriptions are posted
    /// only while the round-robin gate is open. First-time publish posts are
    /// bounded by `max_pending_publish_depth`; already-posted pending txs
    /// may still be reposted for mempool recovery.
    pub(super) fn enqueue_pending_submit(&mut self) {
        if self.resubmit_active || !self.in_flight.is_empty() {
            return;
        }

        let Some(tip) = self.current_tip else {
            self.publish_channel_view();
            return;
        };

        let Some(state) = self.state.as_ref() else {
            self.publish_channel_view();
            return;
        };

        let can_publish_inscription = self.can_publish_inscription_now();
        let pending = state.pending_txs(tip);
        let mut submit = Vec::new();
        let mut active_publish_count = state.posted_pending_publish_count();
        let max_depth = self.config.max_pending_publish_depth.max(1);

        for (id, signed_tx) in pending {
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

        if submit.is_empty() {
            self.publish_channel_view();
            return;
        }

        debug!(target: TARGET, "Submitting {} pending transaction(s)", submit.len());
        let node = self.node.clone();
        self.resubmit_active = true;
        self.in_flight.push(Box::pin(async move {
            let mut results = Vec::with_capacity(submit.len());
            for (id, tx) in submit {
                let result = node
                    .post_transaction(tx)
                    .await
                    .map_err(|err| err.to_string());
                results.push((id, result));
            }
            InFlight::SubmittedBatch { results }
        }));
        self.publish_channel_view();
    }

    pub(super) fn handle_inflight(&mut self, event: InFlight) -> VecDeque<Event> {
        self.resubmit_active = false;
        let mut events = VecDeque::new();

        match event {
            InFlight::SubmittedBatch { results } => {
                for (id, result) in results {
                    if let Err(err) = result {
                        warn!(target: TARGET, "Failed to submit pending transaction {}: {err}", hex::encode(id.0));
                        continue;
                    }

                    let first_post = self
                        .state
                        .as_mut()
                        .is_some_and(|state| state.mark_pending_inscription_posted(&id));
                    if first_post && let Some(event) = self.published_event(id) {
                        events.push_back(event);
                    }
                }
            }
        }

        self.publish_channel_view();
        events
    }

    fn published_event(&self, id: InscriptionId) -> Option<Event> {
        let state = self.state.as_ref()?;
        let pending = state.pending_inscription(&id)?;
        let info = InscriptionInfo {
            tx_hash: pending.tx_hash,
            parent_msg: pending.parent_msg,
            this_msg: pending.this_msg,
            payload: pending.payload.clone(),
        };
        let tx = match pending.withdraws.clone() {
            Some(withdraws) => PublishedTx::AtomicWithdraw(AtomicWithdrawInfo {
                tx_hash: pending.tx_hash,
                inscription: info,
                withdraws,
            }),
            None => PublishedTx::Inscription(info),
        };

        Some(Event::Published { tx: Box::new(tx) })
    }

    /// Process a `BlockEventResult`: apply channel updates to local state and
    /// return the resulting block-derived events in emission order.
    ///
    /// This does not broadcast or buffer events. The caller owns event-delivery
    /// policy because block processing may be combined with readiness
    /// transitions and queued publish draining.
    fn apply_block_result(&mut self, result: BlockEventResult) -> VecDeque<Event> {
        if let Some(update) = result.channel_update.as_ref() {
            Self::log_channel_update(update);

            let has_pending = self
                .state
                .as_ref()
                .is_some_and(TxState::has_pending_inscriptions);

            if !update.orphaned.is_empty() || !has_pending {
                self.last_msg_id = update.new_channel_tip;
            }
        }

        let mut events = VecDeque::new();

        if let Some(update) = result.channel_update {
            events.push_back(self.build_channel_event(update));
        }

        if !result.finalized_items.is_empty() {
            events.push_back(Event::TxsFinalized {
                items: result.finalized_items,
            });
        }

        events
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

    /// Build the `ChannelUpdate` event. `orphaned` contains only our own
    /// pending whose original signed tx is permanently invalid — items the
    /// SDK has given up on (parent slot claimed by a competing inscription,
    /// or parent transitively off canonical). Block-delta orphans whose
    /// original tx is still valid (the SDK keeps retrying them) are not
    /// surfaced. `adopted` is filtered against our internal outbox (by
    /// `this_msg`) to exclude inscriptions this instance submitted —
    /// consumers learn about those via `Event::Published`. This outbox match
    /// works under shared-signing-key deployments: each sequencer instance
    /// only tracks what it itself submitted.
    fn build_channel_event(&mut self, u: ChannelUpdateInfo) -> Event {
        let orphaned = match (self.state.as_mut(), self.current_tip) {
            (Some(s), Some(tip)) => s.shed_off_branch_pending(tip),
            _ => Vec::new(),
        };
        let adopted: Vec<InscriptionInfo> = match self.state.as_ref() {
            Some(s) => u
                .adopted
                .into_iter()
                .filter(|i| !s.outbox_contains(i.this_msg))
                .collect(),
            None => u.adopted,
        };

        let typed_orphaned = orphaned.into_iter().map(orphan_from_shed).collect();

        Event::ChannelUpdate {
            orphaned: typed_orphaned,
            adopted,
        }
    }

    pub(super) async fn handle_request(&mut self, request: ActorRequest) -> Option<Event> {
        if !self.is_ready() {
            reject_not_ready(request);
            return None;
        }

        let event = match request {
            ActorRequest::PublishMessage { data } => self.handle_publish(data),
            ActorRequest::PrepareTx { ops, msg, reply } => {
                let result = prepare_tx(
                    ops,
                    self.channel_id,
                    &self.signing_key,
                    msg,
                    self.last_msg_id,
                );
                // do not update last_msg_id since tx is not submitted yet
                drop(reply.send(Ok(result)));
                None
            }
            ActorRequest::SignTx { tx_hash, reply } => {
                let signature = sign_tx(tx_hash, &self.signing_key);
                drop(reply.send(Ok(signature)));
                None
            }
            ActorRequest::SubmitSignedTx { tx, msg_id, reply } => {
                // Safe to unwrap — is_ready() guarantees state is initialized
                let s = self.state.as_mut().unwrap();
                let result = submit_signed_tx(s, tx, msg_id, &mut self.last_msg_id);
                drop(reply.send(Ok(result)));
                None
            }
            ActorRequest::ChannelConfig {
                keys,
                posting_timeframe,
                posting_timeout,
                configuration_threshold,
                withdraw_threshold,
                reply,
            } => {
                // Safe to unwrap — is_ready() guarantees state is initialized
                let s = self.state.as_mut().unwrap();
                let signed_tx = create_channel_config_tx(
                    self.channel_id,
                    &[&self.signing_key],
                    keys,
                    posting_timeframe,
                    posting_timeout,
                    configuration_threshold,
                    withdraw_threshold,
                );
                s.submit_other(signed_tx.clone());
                let result = PublishResult {
                    inscription_id: signed_tx.mantle_tx.hash(),
                };
                drop(reply.send(Ok((signed_tx, result))));
                self.publish_channel_view();
                None
            }
            ActorRequest::PublishAtomicWithdraw {
                inscribe,
                withdraws,
            } => match self
                .handle_publish_atomic_withdraw(inscribe, withdraws)
                .await
            {
                Ok(event) => event,
                Err(e) => {
                    warn!(target: TARGET, "publish_atomic_withdraw failed: {e}");
                    None
                }
            },
        };
        // After handling a request, publish the latest checkpoint so callers
        // observing via `handle.subscribe_checkpoint()` / `handle.checkpoint()`
        // see post-request state without waiting for the next block. Cheap
        // even on no-op requests (`PrepareTx` / `SignTx`).
        drop(self.publish_checkpoint());
        event
    }

    fn handle_publish(&mut self, data: Inscription) -> Option<Event> {
        self.build_pending_publish(data);
        self.enqueue_pending_submit();
        None
    }

    fn build_pending_publish(&mut self, data: Inscription) -> InscriptionId {
        let parent = {
            let state = self.state.as_mut().unwrap();
            if let Some(tip) = self.current_tip {
                state.publish_parent(tip)
            } else {
                self.last_msg_id
            }
        };
        let (signed_tx, new_msg_id) =
            create_inscribe_tx(self.channel_id, &self.signing_key, data.clone(), parent);
        let id = signed_tx.mantle_tx.hash();

        debug!(target: TARGET,
            "Prepared publish: payload={:?}, parent={}, msg_id={}, tx={}",
            String::from_utf8_lossy(&data),
            hex::encode(parent.as_ref()),
            hex::encode(new_msg_id.as_ref()),
            hex::encode(id.0),
        );

        let state = self.state.as_mut().unwrap();
        state.submit_inscription(signed_tx, parent, new_msg_id, data);
        self.last_msg_id = new_msg_id;

        id
    }

    /// Build, sign, and submit an atomic inscription+withdraw bundle.
    ///
    /// Scoped to centralized single-sequencer channels — the sequencer's own
    /// signature is the only signature used. Errors early if the channel's
    /// `withdraw_threshold > 1`, which would require multi-sig orchestration
    /// not supported by this API.
    async fn handle_publish_atomic_withdraw(
        &mut self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    ) -> Result<Option<Event>, Error> {
        if withdraws.is_empty() {
            return Err(Error::Network(
                "publish_atomic_withdraw requires at least one withdraw".into(),
            ));
        }

        // Query channel state for the current on-chain `withdraw_nonce` and
        // this sequencer's accredited-key index. Done before borrowing
        // `self.state` since `await` on a node method must not hold a `&Self`
        // reference (forces `Self: Sync`).
        let channel_state = self
            .node
            .channel_state(self.channel_id)
            .await
            .map_err(|e| Error::Network(format!("channel_state query failed: {e}")))?
            .ok_or_else(|| {
                Error::Network(format!(
                    "publish_atomic_withdraw requires channel state for {:?}",
                    self.channel_id
                ))
            })?;
        if channel_state.withdraw_threshold > 1 {
            return Err(Error::Network(format!(
                "publish_atomic_withdraw requires withdraw_threshold == 1, got {}",
                channel_state.withdraw_threshold
            )));
        }
        let own_key_index = find_own_key_index(&channel_state, &self.signing_key)?;
        let mut next_nonce = channel_state.withdrawal_nonce;

        // Safe to unwrap — is_ready() guarantees state is initialized
        let s = self.state.as_ref().unwrap();
        let parent = if let Some(tip) = self.current_tip {
            s.publish_parent(tip)
        } else {
            self.last_msg_id
        };

        let mut ops: Vec<Op> = Vec::with_capacity(withdraws.len() + 1);
        let mut withdraw_ops = Vec::with_capacity(withdraws.len());
        for arg in withdraws {
            let op = ChannelWithdrawOp {
                channel_id: self.channel_id,
                outputs: arg.outputs,
                withdraw_nonce: next_nonce,
            };
            withdraw_ops.push(op.clone());
            ops.push(Op::ChannelWithdraw(op));
            next_nonce = next_nonce
                .checked_add(1)
                .ok_or_else(|| Error::Network("withdraw nonce overflow".into()))?;
        }

        let inscription_op = InscriptionOp {
            channel_id: self.channel_id,
            inscription: inscribe.clone(),
            parent,
            signer: self.signing_key.public_key(),
        };
        let msg_id = inscription_op.id();
        ops.push(Op::ChannelInscribe(inscription_op));

        let tx = MantleTx(Ops::try_from(ops).map_err(|e| {
            Error::Network(format!("atomic withdraw bundle exceeds op limit: {e:?}"))
        })?);
        let own_sig = sign_tx(tx.hash(), &self.signing_key);
        let ops_proofs = build_atomic_withdraw_ops_proofs(&tx, own_key_index, own_sig)?;
        let signed_tx = SignedMantleTx::new(tx, ops_proofs)
            .map_err(|e| Error::Network(format!("signed tx assembly failed: {e:?}")))?;

        // Safe to unwrap — is_ready() guarantees state is initialized
        let s = self.state.as_mut().unwrap();

        let tx_hash = signed_tx.mantle_tx.hash();
        let withdraw_infos: Vec<WithdrawInfo> = withdraw_ops
            .into_iter()
            .map(|op| WithdrawInfo { tx_hash, op })
            .collect();
        s.submit_atomic_withdraw(signed_tx, parent, msg_id, inscribe, withdraw_infos);
        self.last_msg_id = msg_id;

        self.enqueue_pending_submit();
        Ok(None)
    }
}

fn reject_not_ready(request: ActorRequest) {
    let err = || Error::Unavailable {
        reason: "sequencer not yet ready",
    };
    match request {
        ActorRequest::PublishMessage { .. } | ActorRequest::PublishAtomicWithdraw { .. } => {
            warn!(target: TARGET, "Publish dropped: sequencer not yet ready");
        }
        ActorRequest::ChannelConfig { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::PrepareTx { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::SignTx { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::SubmitSignedTx { reply, .. } => drop(reply.send(Err(err()))),
    }
}

fn submit_signed_tx(
    state: &mut TxState,
    tx: SignedMantleTx,
    msg_id: MsgId,
    last_msg_id: &mut MsgId,
) -> PublishResult {
    let id = tx.mantle_tx.hash();
    state.submit_other(tx);
    *last_msg_id = msg_id;
    PublishResult { inscription_id: id }
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
            Note, Utxo,
            ledger::{Inputs, Outputs},
            ops::{
                OpProof,
                channel::{ChannelId, config::Keys, deposit::DepositOp},
            },
        },
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_http_api_common::queries::BlocksStreamQuery;
    use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature, ZkKey};
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};
    use tokio::sync::mpsc;

    use super::{
        super::{
            types::{FinalizedOp, FinalizedTx},
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
        let (mut sequencer, handle) = ZoneSequencer::init(channel_id, sequencer_key, node, None);

        // Drive sequencer until ready
        loop {
            if matches!(
                sequencer.next_event().await,
                Some(Event::Readiness { ready: true })
            ) {
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

        // Prepare a `MantleTx` — drive sequencer concurrently to process the request
        let prepare_fut = handle.prepare_tx(
            [Op::ChannelDeposit(deposit_op.clone())].into(),
            b"Mint 10 to Alice".into(),
        );
        tokio::pin!(prepare_fut);
        let (tx, msg_id, inscription_sig) = loop {
            tokio::select! {
                result = &mut prepare_fut => break result.unwrap(),
                _ = sequencer.next_event() => {}
            }
        };
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

        // Submit the signed tx — drive sequencer concurrently to process
        let submit_fut = handle.submit_signed_tx(signed_tx.clone(), msg_id);
        tokio::pin!(submit_fut);
        let result = loop {
            tokio::select! {
                result = &mut submit_fut => break result.unwrap(),
                _ = sequencer.next_event() => {}
            }
        };
        assert_eq!(result.inscription_id, signed_tx.mantle_tx.hash());
        assert_eq!(sequencer.checkpoint().unwrap().last_msg_id, msg_id);
        assert_eq!(posted_txs.recv().await.unwrap(), signed_tx);
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
    /// in `Event::TxsFinalized`. Regression guard for the off-by-one fix
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
        let (mut sequencer, _handle) = ZoneSequencer::init(channel_id, sequencer_key, node, None);

        let mut finalized_items: Vec<FinalizedTx> = Vec::new();
        loop {
            match sequencer.next_event().await {
                Some(Event::Readiness { ready: true }) => break,
                Some(Event::TxsFinalized { items }) => finalized_items.extend(items),
                Some(_) | None => {}
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
