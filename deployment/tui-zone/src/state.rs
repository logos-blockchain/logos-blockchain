use lb_zone_sdk::sequencer::{InscriptionInfo, SequencerCheckpoint};

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

// Keep track of Zone state in memory
//
// published: Inscriptions published by your sequencer, not yet finalised
// finalized: All finalised inscriptions
// checkpoint: Last message in Zone state—lets the sequencer resume after a
// restart
#[derive(Default)]
pub struct InMemoryZoneState {
    published: Vec<Msg>,
    finalized: Vec<Msg>,
    checkpoint: Option<SequencerCheckpoint>,
}

impl InMemoryZoneState {
    // Record a tx we just published locally, so the local view stays in
    // sync with what the SDK accepted. Called at the publish-call site.
    pub fn on_published(&mut self, info: &InscriptionInfo) {
        self.published
            .push(Msg::from_payload(info.this_msg, &info.payload));
    }
}

impl ZoneState for InMemoryZoneState {
    // Move our finalised publishes out of `published` and into `finalized`.
    fn on_finalized(&mut self, inscriptions: &[InscriptionInfo]) {
        for info in inscriptions {
            if let Some(i) = self
                .published
                .iter()
                .position(|m| m.msg_id == info.this_msg)
            {
                self.published.remove(i);
            }
            if !self.finalized.iter().any(|m| m.msg_id == info.this_msg) {
                self.finalized
                    .push(Msg::from_payload(info.this_msg, &info.payload));
            }
        }
    }

    fn published(&self) -> &[Msg] {
        &self.published
    }

    fn finalized(&self) -> &[Msg] {
        &self.finalized
    }

    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint) {
        self.checkpoint = Some(checkpoint);
    }

    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint> {
        self.checkpoint.as_ref()
    }
}
