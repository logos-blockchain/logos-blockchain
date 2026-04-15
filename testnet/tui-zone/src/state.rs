use lb_zone_sdk::{sequencer::SequencerCheckpoint, state::InscriptionInfo};
use uuid::Uuid;

use crate::message::AppMessage;

/// Trait for zone state management.
///
/// The sequencer surfaces chain events (reorgs, finalization); the application
/// maintains its own view of the world by implementing this trait.
///
/// A production implementation might use a database. This demo uses in-memory
/// vecs.
pub trait ZoneState {
    /// Apply a message to the canonical (unfinalized) state.
    fn apply(&mut self, msg: AppMessage);

    /// Revert a message from canonical state (orphaned by reorg).
    fn revert(&mut self, tx_id: &Uuid);

    /// Check if a message with this tx_id exists in canonical or finalized
    /// state.
    fn contains(&self, tx_id: &Uuid) -> bool;

    /// Move inscriptions to finalized state by their payload.
    fn finalize(&mut self, payloads: &[Vec<u8>]);

    /// Current canonical (unfinalized) messages.
    fn canonical(&self) -> &[AppMessage];

    /// Save a sequencer checkpoint.
    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint);

    /// Load the last saved checkpoint.
    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint>;
}

/// In-memory implementation of [`ZoneState`].
#[derive(Default)]
pub struct InMemoryZoneState {
    canonical: Vec<AppMessage>,
    finalized: Vec<AppMessage>,
    checkpoint: Option<SequencerCheckpoint>,
}

impl ZoneState for InMemoryZoneState {
    fn apply(&mut self, msg: AppMessage) {
        if !self.contains(&msg.tx_id) {
            self.canonical.push(msg);
        }
    }

    fn revert(&mut self, tx_id: &Uuid) {
        self.canonical.retain(|m| &m.tx_id != tx_id);
    }

    fn contains(&self, tx_id: &Uuid) -> bool {
        self.canonical.iter().any(|m| &m.tx_id == tx_id)
            || self.finalized.iter().any(|m| &m.tx_id == tx_id)
    }

    fn finalize(&mut self, payloads: &[Vec<u8>]) {
        for payload in payloads {
            if let Some(msg) = AppMessage::from_bytes(payload) {
                self.canonical.retain(|m| m.tx_id != msg.tx_id);
                if !self.finalized.iter().any(|m| m.tx_id == msg.tx_id) {
                    self.finalized.push(msg);
                }
            }
        }
    }

    fn canonical(&self) -> &[AppMessage] {
        &self.canonical
    }

    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint) {
        self.checkpoint = Some(checkpoint);
    }

    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint> {
        self.checkpoint.as_ref()
    }
}

/// Process a channel update event: revert invalidated messages, apply adopted
/// ones, and return the list of messages that need to be re-published.
///
/// This is the core of conflict resolution:
/// 1. Revert all invalidated inscriptions from our state
/// 2. Apply all adopted inscriptions to our state
/// 3. For each invalidated message whose tx_id is NOT in our state after the
///    update — it was truly lost and needs re-publishing
///
/// A real sequencer might add additional checks here (e.g., skip re-publishing
/// if a swap became unprofitable on the new branch).
pub fn resolve_conflicts(
    state: &mut dyn ZoneState,
    invalidated: &[InscriptionInfo],
    adopted: &[InscriptionInfo],
) -> Vec<AppMessage> {
    // Step 1: revert invalidated
    for inv in invalidated {
        if let Some(msg) = AppMessage::from_bytes(&inv.payload) {
            state.revert(&msg.tx_id);
        }
    }

    // Step 2: apply adopted
    for adp in adopted {
        if let Some(msg) = AppMessage::from_bytes(&adp.payload) {
            state.apply(msg);
        }
    }

    // Step 3: collect messages that need re-publishing
    let mut to_republish = Vec::new();
    for inv in invalidated {
        if let Some(msg) = AppMessage::from_bytes(&inv.payload) {
            if !state.contains(&msg.tx_id) {
                to_republish.push(msg);
            }
        }
    }
    to_republish
}
