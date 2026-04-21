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
    fn revert(&mut self, tx_uuid: &Uuid);

    /// Check if a message with this `tx_uuid` exists in canonical or finalized
    /// state.
    fn contains(&self, tx_uuid: &Uuid) -> bool;

    /// Move inscriptions to finalized state by their payload.
    fn finalize(&mut self, payloads: &[Vec<u8>]);

    /// Current canonical (unfinalized) messages.
    fn canonical(&self) -> &[AppMessage];

    /// Finalized messages (below LIB, immutable).
    fn finalized(&self) -> &[AppMessage];

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
        if !self.contains(&msg.tx_uuid) {
            self.canonical.push(msg);
        }
    }

    fn revert(&mut self, tx_uuid: &Uuid) {
        self.canonical.retain(|m| &m.tx_uuid != tx_uuid);
    }

    fn contains(&self, tx_uuid: &Uuid) -> bool {
        self.canonical.iter().any(|m| &m.tx_uuid == tx_uuid)
            || self.finalized.iter().any(|m| &m.tx_uuid == tx_uuid)
    }

    fn finalize(&mut self, payloads: &[Vec<u8>]) {
        for payload in payloads {
            if let Some(msg) = AppMessage::from_bytes(payload) {
                self.canonical.retain(|m| m.tx_uuid != msg.tx_uuid);
                if !self.finalized.iter().any(|m| m.tx_uuid == msg.tx_uuid) {
                    self.finalized.push(msg);
                }
            }
        }
    }

    fn canonical(&self) -> &[AppMessage] {
        &self.canonical
    }

    fn finalized(&self) -> &[AppMessage] {
        &self.finalized
    }

    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint) {
        self.checkpoint = Some(checkpoint);
    }

    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint> {
        self.checkpoint.as_ref()
    }
}

/// Process a channel update event: revert orphaned messages, apply adopted
/// ones, and return the list of messages that need to be re-published.
///
/// This is the core of conflict resolution:
/// 1. Revert all orphaned inscriptions from our state
/// 2. Apply all adopted inscriptions to our state
/// 3. For each orphaned message whose `tx_uuid` is NOT in our state after the
///    update — it was truly lost and needs re-publishing
///
/// A real sequencer might add additional checks here (e.g., skip re-publishing
/// if a swap became unprofitable on the new branch).
pub fn resolve_conflicts(
    state: &mut dyn ZoneState,
    orphaned: &[InscriptionInfo],
    adopted: &[InscriptionInfo],
) -> Vec<AppMessage> {
    // Step 1: revert orphaned
    for inv in orphaned {
        if let Some(msg) = AppMessage::from_bytes(&inv.payload) {
            state.revert(&msg.tx_uuid);
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
    for inv in orphaned {
        if let Some(msg) = AppMessage::from_bytes(&inv.payload)
            && !state.contains(&msg.tx_uuid)
        {
            to_republish.push(msg);
        }
    }
    to_republish
}
