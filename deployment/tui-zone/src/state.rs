use lb_core::mantle::ops::channel::MsgId;
use lb_zone_sdk::sequencer::{InscriptionInfo, SequencerChannelView, SequencerCheckpoint};

use crate::message::Msg;

/// Trait for the TUI's view of zone state.
///
/// The TUI feeds SDK events into this trait; the trait owns persistence.
/// `InMemoryZoneState` is the demo implementation. A real sequencer would
/// implement it over a DB so `published`/`finalized` survive
/// restarts (the SDK's own checkpoint covers tx-level resume separately).
///
/// Three lists, each ordered by arrival:
/// - `published`: our submissions, in submit order, until they finalize or get
///   orphaned.
/// - `finalized`: all inscriptions below LIB, in canonical order — the SDK
///   delivers `finalized` on `BlocksProcessed`.
///
/// Replay-idempotent: `on_finalized` dedup by `msg_id`, so
/// resuming from a persisted state and re-receiving backfill is harmless.
pub trait ZoneState: Send {
    fn on_finalized(&mut self, inscriptions: &[InscriptionInfo]);

    fn published(&self) -> &[Msg];
    fn finalized(&self) -> &[Msg];

    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint);
    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint>;
}

// Your Code Here
