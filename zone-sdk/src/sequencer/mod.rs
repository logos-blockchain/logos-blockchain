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
//!   live on the drive task. Synchronous; every state-mutating method returns
//!   the resulting [`SequencerCheckpoint`] inline for atomic outbox +
//!   checkpoint persistence.
//! - [`SequencerClient`] is the cross-task command surface — same set of
//!   publish/admin methods as [`SequencerHandle`] plus the subscription
//!   accessors. Obtained from [`ZoneSequencer::client`]; cheap-to-clone, can be
//!   moved into any task. Routes commands through an internal channel processed
//!   inside [`ZoneSequencer::next_event`], so the drive loop must still be
//!   polled for client publish awaits to resolve. Subscriptions work regardless
//!   of poll state.
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
//!         ev = sequencer.next_event() => {
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
//! If publish calls need to originate from tasks other than the drive
//! loop — e.g. an HTTP request handler or a service-style integration —
//! obtain a [`SequencerClient`] via [`ZoneSequencer::client`] before moving
//! the sequencer into its drive task and clone it into the calling tasks.
//! The client's `publish(...).await` resolves once the drive loop processes
//! the command, with the same `(PublishResult, SequencerCheckpoint)` return
//! as the synchronous handle.
//!
//! # Restart
//!
//! Persist [`SequencerCheckpoint`] received via [`Event::BlocksProcessed`]
//! (or directly from a publish call). Pass the most recent checkpoint back
//! to [`ZoneSequencer::init`] to resume.

mod actor;
mod backfill;
mod block_fetch;
mod client;
mod handle;
mod slot_clock;
mod state;
mod tx_builder;
mod types;
mod zone_sequencer;

pub(super) const TARGET: &str = lb_log_targets::zone_sdk::SEQUENCER;

pub use block_fetch::channel_inscriptions;
pub use client::SequencerClient;
pub use handle::SequencerHandle;
pub use types::{
    AtomicWithdrawInfo, ChannelUpdate, ChannelUpdateTx, DepositInfo, Error, Event, FinalizedOp,
    FinalizedTx, FundingConfig, InscriptionId, InscriptionInfo, PendingTx, PublishResult,
    SequencerChannelView, SequencerCheckpoint, SequencerConfig, TurnNotification, TxSource,
    TxStatus, TxStatusUpdate, WithdrawArg, WithdrawInfo,
};
pub use zone_sequencer::ZoneSequencer;
