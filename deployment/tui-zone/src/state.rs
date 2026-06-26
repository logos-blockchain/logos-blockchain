use std::collections::HashSet;

use lb_zone_sdk::sequencer::{InscriptionInfo, SequencerChannelView, SequencerCheckpoint};

use crate::message::Msg;

/// Trait for the TUI's view of zone state.
///
/// The TUI feeds SDK events into this trait; the trait owns persistence.
/// `InMemoryZoneState` is the demo implementation.
///
/// Tracks two lists:
/// - `pending`: messages we published that haven't finalized yet.
/// - `finalized`: inscriptions below LIB, delivered on `BlocksProcessed`.
///
/// The SDK manages the outbox (resubmit and shed across reorgs); this state
/// renders published and finalized messages and does not consume the channel
/// delta.
pub trait ZoneState: Send {
    /// Record a message we just published as pending.
    fn on_published(&mut self, info: &InscriptionInfo);
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
    /// Payload bytes of finalized inscriptions, pinned permanently. Keyed on
    /// the raw payload (which carries the `tx_uuid`) so a finalized payload
    /// is never re-published when a losing competitor is later orphaned.
    finalized_payloads: HashSet<Vec<u8>>,
    checkpoint: Option<SequencerCheckpoint>,
    channel_view: Option<SequencerChannelView>,
}

impl ZoneState for InMemoryZoneState {
    fn on_published(&mut self, info: &InscriptionInfo) {
        if !self.pending.iter().any(|m| m.msg_id == info.this_msg) {
            self.pending
                .push(Msg::from_payload(info.this_msg, &info.payload));
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
            self.finalized_payloads
                .insert(info.payload.as_slice().to_vec());
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

    /// True if this exact payload has finalized — used to skip re-publishing an
    /// orphan whose payload is already permanently on chain.
    #[must_use]
    pub fn is_finalized(&self, payload: &[u8]) -> bool {
        self.finalized_payloads.contains(payload)
    }
}
