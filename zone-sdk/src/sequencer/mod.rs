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
//! - [`SequencerHandle`] is the cloneable async command surface — publish,
//!   prepare/sign/submit, channel config. Safe to use from any task.
//!
//! # Driving the sequencer
//!
//! Construct via [`ZoneSequencer::init`] (or
//! [`ZoneSequencer::init_with_config`] for non-default config) and pump
//! events with [`ZoneSequencer::events`] in a loop. From other tasks, submit
//! publishes through the returned [`SequencerHandle`]:
//!
//! - [`SequencerHandle::publish_message`] — fire-and-forget inscription.
//! - [`SequencerHandle::publish_atomic_withdraw`] — bundled
//!   inscription+withdraw.
//! - [`SequencerHandle::channel_config`] — update channel config.
//!
//! Results land asynchronously on the drive loop as [`Event::Published`] /
//! [`Event::TxsFinalized`] / [`Event::ChannelUpdate`] / [`Event::Checkpoint`].
//!
//! The [`ZoneSequencer::events`] stream borrows the sequencer mutably for its
//! lifetime; the borrow checker therefore enforces "drive xor inspect" — to
//! query state or send commands while driving, use the [`SequencerHandle`]
//! from another task.
//!
//! # Restart
//!
//! Persist [`SequencerCheckpoint`] — received either via [`Event::Checkpoint`]
//! on the drive task's event stream, or via [`ZoneSequencer::checkpoint`] /
//! [`ZoneSequencer::subscribe_checkpoint`] (subscribe before the spawn; the
//! receiver keeps working from any task afterwards). Pass it back to
//! [`ZoneSequencer::init`] to resume.

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
    AtomicWithdrawInfo, DepositInfo, Error, Event, FinalizedOp, FinalizedTx, InscriptionId,
    InscriptionInfo, OrphanedTx, PublishResult, PublishedTx, SequencerChannelView,
    SequencerCheckpoint, SequencerConfig, TurnNotification, WithdrawArg, WithdrawInfo,
};
pub use zone_sequencer::ZoneSequencer;
