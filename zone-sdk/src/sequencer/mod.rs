//! Zone channel sequencer.
//!
//! Drive a single channel sequencer that publishes inscriptions, tracks
//! pending and finalized state, handles reconnects and chain reorgs, and
//! surfaces a stream of [`Event`]s to the caller.
//!
//! # Roles
//!
//! - [`ZoneSequencer`] drives the state machine, emits [`Event`]s on its
//!   stream, and exposes snapshot reads and watch subscriptions for current
//!   state ([`ZoneSequencer::is_ready`], [`ZoneSequencer::checkpoint`],
//!   [`ZoneSequencer::subscribe_ready`], etc.).
//! - [`SequencerHandle`] is the drive-loop command surface — publish,
//!   prepare/sign/submit, channel config. Obtained from
//!   [`ZoneSequencer::handle`]; borrows the sequencer mutably so it can only
//!   live on the drive task. Every state-mutating method returns the resulting
//!   [`SequencerCheckpoint`] inline for atomic outbox + checkpoint persistence.
//!
//! # Driving the sequencer
//!
//! Construct via [`ZoneSequencer::init`] (or
//! [`ZoneSequencer::init_with_config`]) and pump events with
//! [`ZoneSequencer::next_event`] inside a `tokio::select!`:
//!
//! ```ignore
//! loop {
//!     tokio::select! {
//!         Some(msg) = ui_rx.recv() => {
//!             let (result, cp) = sequencer.handle().publish(msg)?;
//!             db.tx(|t| { t.outbox(result); t.save_checkpoint(cp); });
//!         }
//!         Some(ev) = sequencer.next_event() => {
//!             handle_event(ev, &mut sequencer, &mut db).await;
//!         }
//!     }
//! }
//! ```
//!
//! Inside `handle_event` you have `&mut sequencer` and can call
//! `sequencer.handle().publish(...)` to (e.g.) re-publish orphans surfaced
//! by [`ChannelUpdate`].
//!
//! # Restart
//!
//! Persist [`SequencerCheckpoint`] received via [`Event::BlocksProcessed`]
//! (or directly from a publish call). Pass the most recent checkpoint back
//! to [`ZoneSequencer::init`] to resume.

mod actor;
mod backfill;
mod block_fetch;
mod handle;
mod slot_clock;
mod state;
mod tx_builder;
mod types;
mod zone_sequencer;

pub(super) const TARGET: &str = lb_log_targets::zone_sdk::SEQUENCER;

pub use handle::SequencerHandle;
pub use types::{
    AtomicWithdrawInfo, ChannelUpdate, DepositInfo, Error, Event, FinalizedOp, FinalizedTx,
    InscriptionId, InscriptionInfo, OrphanedTx, PendingTx, PublishResult, SequencerChannelView,
    SequencerCheckpoint, SequencerConfig, TurnNotification, TxSource, TxStatus, TxStatusUpdate,
    WithdrawArg, WithdrawInfo,
};
pub use zone_sequencer::ZoneSequencer;
