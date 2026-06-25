use lb_core::mantle::ops::channel::MsgId;
use lb_zone_sdk::sequencer::{InscriptionInfo, SequencerChannelView, SequencerCheckpoint};

use crate::message::Msg;

/// Trait for the TUI's view of zone state.
///
/// The TUI feeds SDK events into this trait; the trait owns persistence.
/// `InMemoryZoneState` is the demo implementation. A real sequencer would
/// implement it over a DB so the channel view survives restarts (the SDK's own
/// checkpoint covers tx-level resume separately).
///
/// Two lists modelling the linear channel message chain, each in canonical
/// order:
/// - `pending`: inscriptions on the canonical channel above LIB (not yet
///   finalized), deduped by `msg_id`. Built from `adopted`/`orphaned`.
/// - `finalized`: inscriptions below LIB — the SDK delivers these on
///   `BlocksProcessed`.
///
/// The TUI does not track its own outbox; the SDK manages pending submissions.
/// This state just mirrors the canonical channel chain for rendering.
pub trait ZoneState: Send {
    /// Add newly-canonical inscriptions to `pending` (deduped by `msg_id`).
    fn on_adopted(&mut self, adopted: &[InscriptionInfo]);
    /// Remove an inscription that fell off the canonical chain from `pending`.
    fn on_orphaned(&mut self, msg_id: &MsgId);
    /// Move finalized inscriptions from `pending` into `finalized`.
    fn on_finalized(&mut self, inscriptions: &[InscriptionInfo]);

    fn pending(&self) -> &[Msg];
    fn finalized(&self) -> &[Msg];

    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint);
    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint>;
}

/// In-memory implementation of [`ZoneState`].
#[derive(Default)]
pub struct InMemoryZoneState {
    pending: Vec<Msg>,
    finalized: Vec<Msg>,
    checkpoint: Option<SequencerCheckpoint>,
    channel_view: Option<SequencerChannelView>,
}

impl ZoneState for InMemoryZoneState {
    fn on_adopted(&mut self, adopted: &[InscriptionInfo]) {
        for info in adopted {
            if !self.pending.iter().any(|m| m.msg_id == info.this_msg) {
                self.pending
                    .push(Msg::from_payload(info.this_msg, &info.payload));
            }
        }
    }

    fn on_orphaned(&mut self, msg_id: &MsgId) {
        if let Some(i) = self.pending.iter().position(|m| &m.msg_id == msg_id) {
            self.pending.remove(i);
        }
    }

    fn on_finalized(&mut self, inscriptions: &[InscriptionInfo]) {
        for info in inscriptions {
            if let Some(i) = self.pending.iter().position(|m| m.msg_id == info.this_msg) {
                self.pending.remove(i);
            }
            if !self.finalized.iter().any(|m| m.msg_id == info.this_msg) {
                self.finalized
                    .push(Msg::from_payload(info.this_msg, &info.payload));
            }
        }
    }

    fn pending(&self) -> &[Msg] {
        &self.pending
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
}
