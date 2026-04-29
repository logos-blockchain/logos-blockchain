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

impl InMemoryZoneState {
    /// Find a stored message by uuid in canonical or finalized. Used to
    /// read back `is_ours` for a message we've seen before.
    pub fn get(&self, tx_uuid: &Uuid) -> Option<&AppMessage> {
        self.canonical
            .iter()
            .chain(self.finalized.iter())
            .find(|m| &m.tx_uuid == tx_uuid)
    }
}

impl ZoneState for InMemoryZoneState {
    fn apply(&mut self, msg: AppMessage) {
        // Preserve `is_ours` on re-apply: if we already stored this uuid
        // (e.g. optimistically on publish), do not overwrite with a
        // chain-decoded copy whose `is_ours` is false.
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
                // Move from canonical to finalized, preserving is_ours.
                let existing = self
                    .canonical
                    .iter()
                    .position(|m| m.tx_uuid == msg.tx_uuid)
                    .map(|i| self.canonical.remove(i));
                if !self.finalized.iter().any(|m| m.tx_uuid == msg.tx_uuid) {
                    self.finalized.push(existing.unwrap_or(msg));
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

/// Process a channel update event.
///
/// 1. Revert `orphaned` from state.
/// 2. Apply `adopted` to state.
/// 3. Re-publish each `invalidated` entry that is ours, not currently in state,
///    and not in `pending`.
pub fn resolve_conflicts(
    state: &mut InMemoryZoneState,
    orphaned: &[InscriptionInfo],
    adopted: &[InscriptionInfo],
    pending: &[InscriptionInfo],
    invalidated: &[InscriptionInfo],
) -> Vec<AppMessage> {
    // Capture our messages from `invalidated` BEFORE reverting orphans —
    // `is_ours` lives on the stored AppMessage and revert removes it.
    let mut ours: Vec<AppMessage> = Vec::new();
    for inv in invalidated {
        if let Some(decoded) = AppMessage::from_bytes(&inv.payload)
            && let Some(stored) = state.get(&decoded.tx_uuid)
            && stored.is_ours
        {
            ours.push(stored.clone());
        }
    }

    for inv in orphaned {
        if let Some(msg) = AppMessage::from_bytes(&inv.payload) {
            state.revert(&msg.tx_uuid);
        }
    }

    for adp in adopted {
        if let Some(msg) = AppMessage::from_bytes(&adp.payload) {
            state.apply(msg);
        }
    }

    let pending_uuids: std::collections::HashSet<Uuid> = pending
        .iter()
        .filter_map(|inv| AppMessage::from_bytes(&inv.payload).map(|m| m.tx_uuid))
        .collect();

    ours.into_iter()
        .filter(|m| !state.contains(&m.tx_uuid) && !pending_uuids.contains(&m.tx_uuid))
        .collect()
}
