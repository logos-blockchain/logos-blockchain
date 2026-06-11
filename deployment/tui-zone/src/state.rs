use lb_core::mantle::ops::channel::MsgId;
use lb_zone_sdk::sequencer::{InscriptionInfo, SequencerChannelView, SequencerCheckpoint};

use crate::message::Msg;

/// Trait for the TUI's view of zone state.
///
/// The TUI feeds SDK events into this trait; the trait owns persistence.
/// `InMemoryZoneState` is the demo implementation. A real sequencer would
/// implement it over a DB so `published`/`adopted`/`finalized` survive
/// restarts (the SDK's own checkpoint covers tx-level resume separately).
///
/// Three lists, each ordered by arrival:
/// - `published`: our submissions, in submit order, until they finalize or get
///   orphaned.
/// - `adopted`: others' inscriptions on canonical, deduped by `msg_id` (reorgs
///   can re-adopt the same one), in first-sighting order.
/// - `finalized`: all inscriptions below LIB, in canonical order — the SDK
///   delivers `finalized` on `BlocksProcessed`.
///
/// Replay-idempotent: `on_adopted` and `on_finalized` dedup by `msg_id`, so
/// resuming from a persisted state and re-receiving backfill is harmless.
pub trait ZoneState: Send {
    fn on_adopted(&mut self, adopted: &[InscriptionInfo]);
    /// Remove our orphaned entry from `published`. Caller is expected to
    /// auto-republish via `sequencer.handle().publish`.
    fn on_orphaned(&mut self, msg_id: &MsgId);
    fn on_finalized(&mut self, inscriptions: &[InscriptionInfo]);

    fn published(&self) -> &[Msg];
    fn adopted(&self) -> &[Msg];
    fn finalized(&self) -> &[Msg];

    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint);
    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint>;
}

/// In-memory implementation of [`ZoneState`].
#[derive(Default)]
pub struct InMemoryZoneState {
    published: Vec<Msg>,
    adopted: Vec<Msg>,
    finalized: Vec<Msg>,
    checkpoint: Option<SequencerCheckpoint>,
    channel_view: Option<SequencerChannelView>,
}

impl ZoneState for InMemoryZoneState {
    fn on_adopted(&mut self, adopted: &[InscriptionInfo]) {
        for info in adopted {
            if !self.adopted.iter().any(|m| m.msg_id == info.this_msg) {
                self.adopted
                    .push(Msg::from_payload(info.this_msg, &info.payload));
            }
        }
    }

    fn on_orphaned(&mut self, msg_id: &MsgId) {
        if let Some(i) = self.published.iter().position(|m| &m.msg_id == msg_id) {
            self.published.remove(i);
        }
    }

    fn on_finalized(&mut self, inscriptions: &[InscriptionInfo]) {
        for info in inscriptions {
            if let Some(i) = self
                .published
                .iter()
                .position(|m| m.msg_id == info.this_msg)
            {
                self.published.remove(i);
            } else if let Some(i) = self.adopted.iter().position(|m| m.msg_id == info.this_msg) {
                self.adopted.remove(i);
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

    fn adopted(&self) -> &[Msg] {
        &self.adopted
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

impl InMemoryZoneState {
    pub fn set_channel_view(&mut self, channel_view: SequencerChannelView) {
        self.channel_view = Some(channel_view);
    }

    pub const fn channel_view(&self) -> Option<&SequencerChannelView> {
        self.channel_view.as_ref()
    }

    /// Record a tx we just published locally. Called at the publish-call
    /// site so the local outbox stays in sync with what the SDK accepted.
    pub fn on_published(&mut self, info: &InscriptionInfo) {
        self.published
            .push(Msg::from_payload(info.this_msg, &info.payload));
    }
}
