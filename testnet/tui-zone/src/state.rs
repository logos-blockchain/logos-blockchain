use std::collections::HashSet;

use lb_zone_sdk::{sequencer::SequencerCheckpoint, state::InscriptionInfo};
use uuid::Uuid;

use crate::message::AppMessage;

/// Trait for zone state management.
///
/// The sequencer surfaces chain events (reorgs, finalization); the application
/// maintains its own view of the world by implementing this trait.
///
/// Authorship ("did we send this?") is tracked independently of the
/// canonical/finalized stores via `mark_ours` / `is_ours`. This decoupling
/// keeps authorship durable across reorgs that revert and re-apply messages.
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

    /// Record that this `tx_uuid` was locally created. Call before publishing.
    fn mark_ours(&mut self, tx_uuid: Uuid);

    /// Whether this `tx_uuid` was locally created, regardless of whether it
    /// currently exists in canonical/finalized state.
    fn is_ours(&self, tx_uuid: &Uuid) -> bool;
}

/// In-memory implementation of [`ZoneState`].
#[derive(Default)]
pub struct InMemoryZoneState {
    canonical: Vec<AppMessage>,
    finalized: Vec<AppMessage>,
    my_submissions: HashSet<Uuid>,
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

    fn mark_ours(&mut self, tx_uuid: Uuid) {
        self.my_submissions.insert(tx_uuid);
    }

    fn is_ours(&self, tx_uuid: &Uuid) -> bool {
        self.my_submissions.contains(tx_uuid)
    }
}

/// Process a channel update event.
///
/// 1. Revert orphaned from state.
/// 2. Apply adopted to state.
/// 3. Among invalidated, return our messages that are not on the new canonical
///    chain and not already in flight.
///
/// Authorship is read from `state.is_ours`, which is durable across reorgs —
/// so the order is the natural one: mutate state first, then decide based on
/// the new chain.
pub fn resolve_conflicts(
    state: &mut InMemoryZoneState,
    orphaned: &[InscriptionInfo],
    adopted: &[InscriptionInfo],
    pending: &[InscriptionInfo],
    invalidated: &[InscriptionInfo],
) -> Vec<AppMessage> {
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

    let pending_uuids: HashSet<Uuid> = pending
        .iter()
        .filter_map(|inv| AppMessage::from_bytes(&inv.payload).map(|m| m.tx_uuid))
        .collect();

    invalidated
        .iter()
        .filter_map(|inv| AppMessage::from_bytes(&inv.payload))
        .filter(|m| state.is_ours(&m.tx_uuid))
        .filter(|m| !state.contains(&m.tx_uuid))
        .filter(|m| !pending_uuids.contains(&m.tx_uuid))
        .collect()
}
