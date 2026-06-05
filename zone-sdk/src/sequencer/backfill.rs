#![allow(
    clippy::multiple_inherent_impl,
    reason = "`ZoneSequencer` impl is split across actor.rs / backfill.rs / zone_sequencer.rs by concern for navigability."
)]

use lb_common_http_client::{ChainServiceInfo, Slot};
use lb_core::mantle::ops::channel::MsgId;
use tracing::{debug, error, info, warn};

use super::{
    TARGET,
    block_fetch::fetch_and_process_blocks,
    slot_clock::SlotClock,
    state::TxState,
    types::{Event, FinalizedOp},
    zone_sequencer::ZoneSequencer,
};
use crate::adapter;

const BACKFILL_BATCH_SIZE: u64 = 100;

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Process one batch of incremental backfill if active.
    ///
    /// Returns `Some(event)` while backfill is active (caller should return
    /// the inner value), or `None` when backfill is complete/inactive.
    pub(super) async fn process_incremental_backfill(&mut self) -> Option<Option<Event>> {
        let (Some(from), Some(to)) = (self.backfill_from, self.backfill_to) else {
            return None;
        };

        let from_u64: u64 = from.into();
        let to_u64: u64 = to.into();

        if from_u64 > to_u64 {
            // Backfill exhausted — range advanced past `to` in a previous batch.
            self.backfill_from = None;
            self.backfill_to = None;
            return None;
        }

        let batch_end = (from_u64 + BACKFILL_BATCH_SIZE).min(to_u64);
        let batch = match fetch_and_process_blocks(
            self.state.as_mut().unwrap(),
            from_u64,
            batch_end,
            self.channel_id,
            &self.node,
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                error!(
                    target: TARGET,
                    from = from_u64,
                    to = batch_end,
                    "Backfill batch failed; will retry same range after delay: {e}"
                );
                tokio::time::sleep(self.config.reconnect_delay).await;
                // Leave `backfill_from` untouched so the next tick retries
                // the same range. Active but no event this turn.
                return Some(None);
            }
        };

        self.backfill_from = Some(Slot::from(batch_end + 1));

        // Advance the channel-tip marker using the last inscription in the
        // batch. Deposits / withdraws don't have a `this_msg` lineage.
        if let Some(last_inscription) = batch
            .items
            .iter()
            .rev()
            .flat_map(|t| t.ops.iter().rev())
            .find_map(|op| match op {
                FinalizedOp::Inscription(i) => Some(i),
                FinalizedOp::Deposit(_) | FinalizedOp::Withdraw(_) => None,
            })
        {
            self.last_msg_id = last_inscription.this_msg;
            if let Some(s) = self.state.as_mut() {
                s.set_finalized_msg(last_inscription.this_msg);
            }
        }

        // Clean up our pending set for txs that finalized in this batch.
        // Mirrors the cleanup in `handle_block_event`. Without this, restored
        // pending txs whose blocks were already finalized during downtime
        // would leak in `state.pending` and risk being mis-classified as
        // orphaned by `shed_off_branch_pending` once a live block arrives.
        if let Some(s) = self.state.as_mut() {
            for tx_hash in &batch.our_tx_hashes {
                s.remove_pending(tx_hash);
            }
        }

        self.lib_slot = Slot::from(batch_end);

        let checkpoint_event = self
            .publish_checkpoint()
            .map(|checkpoint| Event::Checkpoint { checkpoint });

        if batch.items.is_empty() {
            return checkpoint_event.map(Some);
        }

        let event = Event::TxsFinalized { items: batch.items };
        drop(self.event_tx.send(event.clone()));
        if let Some(cp) = checkpoint_event {
            self.buffered_events.push_back(cp);
        }
        Some(Some(event))
    }

    /// Ensure the blocks stream is connected. Returns `false` if not yet
    /// ready (caller should return `None`).
    pub(super) async fn ensure_connected(&mut self) -> bool {
        if self.blocks_stream.is_some() {
            return true;
        }
        debug!(target: TARGET, "ensure_connected: connecting...");

        if !self.init_state_if_needed().await {
            return false;
        }
        if !self.open_block_stream().await {
            return false;
        }
        if !self.setup_backfill_range().await {
            return false;
        }
        true
    }

    /// Initialize startup-derived sequencer state from consensus info.
    /// Preserves restored `TxState` when resuming from checkpoint, but ensures
    /// the slot clock and initial channel view are available before the
    /// sequencer is considered connected.
    ///
    /// `current_tip` stays None so the first live block event emits everything
    /// from LIB up to the new tip as `adopted`. On reconnect this is a no-op
    /// once both `state` and `slot_clock` are initialized.
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn init_state_if_needed(&mut self) -> bool {
        if self.state.is_some() && self.slot_clock.is_some() {
            return true;
        }
        match self.node.consensus_info().await {
            Ok(ChainServiceInfo {
                cryptarchia_info, ..
            }) => {
                info!(target: TARGET,
                    "Sequencer connected: tip={:?}, lib={:?}",
                    cryptarchia_info.tip, cryptarchia_info.lib
                );
                if let Err(err) = self.refresh_channel_state().await {
                    warn!(target: TARGET, "Failed to fetch initial channel state: {err}");
                    tokio::time::sleep(self.config.reconnect_delay).await;
                    return false;
                }
                if self.state.is_none() {
                    self.state = Some(TxState::new(cryptarchia_info.lib, MsgId::root()));
                }
                self.slot_clock = Some(self.build_initial_slot_clock(cryptarchia_info.slot));
                self.publish_channel_view();
                true
            }
            Err(e) => {
                warn!(target: TARGET, "Failed to fetch consensus info: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                false
            }
        }
    }

    fn build_initial_slot_clock(&self, observed_slot: Slot) -> SlotClock {
        self.config.chain_start_time.map_or_else(
            || SlotClock::from_observed_slot(observed_slot, self.config.slot_duration),
            |chain_start_time| {
                let mut slot_clock =
                    SlotClock::from_chain_start_time(chain_start_time, self.config.slot_duration);
                slot_clock.observe_slot(observed_slot);
                slot_clock
            },
        )
    }

    async fn open_block_stream(&mut self) -> bool {
        debug!(target: TARGET, "ensure_connected: opening blocks stream...");
        match self.node.block_stream().await {
            Ok(stream) => {
                debug!(target: TARGET, "ensure_connected: blocks stream connected");
                self.blocks_stream = Some(stream);
                true
            }
            Err(e) => {
                warn!(target: TARGET, "Failed to connect to blocks stream: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                false
            }
        }
    }

    /// Check whether an incremental backfill range is needed (checkpoint lib
    /// behind current network lib). Returns `false` if a backfill was set up
    /// (caller defers readiness until backfill completes).
    ///
    /// `backfill_from_genesis` selects the inclusive start: on cold start
    /// the range begins at slot 0 so genesis-inscribed channels are picked
    /// up; on a warm restart from a checkpoint, the checkpoint slot is
    /// already processed and the range starts at `from + 1`.
    async fn setup_backfill_range(&mut self) -> bool {
        if self.state.is_none() || self.backfill_from.is_some() {
            return true;
        }
        match self.node.consensus_info().await {
            Ok(ChainServiceInfo {
                cryptarchia_info, ..
            }) => {
                let network_lib_slot = cryptarchia_info.lib_slot;
                let from: u64 = self.lib_slot.into();
                let to: u64 = network_lib_slot.into();
                let (start, run) = if self.backfill_from_genesis {
                    self.backfill_from_genesis = false;
                    (from, true)
                } else {
                    (from + 1, from < to)
                };
                if run {
                    debug!(target: TARGET, "Starting incremental backfill from slot {start} to {to}");
                    self.backfill_from = Some(Slot::from(start));
                    self.backfill_to = Some(network_lib_slot);
                    return false;
                }
                true
            }
            Err(e) => {
                warn!(target: TARGET, "Failed to fetch consensus info for backfill check: {e}");
                true
            }
        }
    }
}
