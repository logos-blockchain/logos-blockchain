use std::collections::{HashMap, HashSet};

use lb_zone_sdk::{sequencer::SequencerCheckpoint, state::InscriptionInfo};
use uuid::Uuid;

use crate::message::AppMessage;

/// Trait for zone state management.
///
/// The sequencer surfaces chain events (reorgs, finalization); the application
/// maintains its own view of the world by implementing this trait.
///
/// Submissions ("things we sent and might still need to retry") are tracked
/// independently of the canonical/finalized stores via `mark_ours`. The SDK
/// does not surface payloads of pending items it has dropped, so we remember
/// our own payloads to drive republish decisions across reorgs.
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

    /// Move inscriptions to finalized state by their payload. Also clears the
    /// finalized entries from the local submission set.
    fn finalize(&mut self, payloads: &[Vec<u8>]);

    /// Current canonical (unfinalized) messages.
    fn canonical(&self) -> &[AppMessage];

    /// Finalized messages (below LIB, immutable).
    fn finalized(&self) -> &[AppMessage];

    /// Save a sequencer checkpoint.
    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint);

    /// Load the last saved checkpoint.
    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint>;

    /// Remember a locally-created submission so we can iterate to make
    /// republish decisions even if the SDK silently drops it.
    fn mark_ours(&mut self, msg: &AppMessage);
}

/// In-memory implementation of [`ZoneState`].
#[derive(Default)]
pub struct InMemoryZoneState {
    canonical: Vec<AppMessage>,
    finalized: Vec<AppMessage>,
    my_submissions: HashMap<Uuid, AppMessage>,
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
                let tx_uuid = msg.tx_uuid;
                let existing = self
                    .canonical
                    .iter()
                    .position(|m| m.tx_uuid == tx_uuid)
                    .map(|i| self.canonical.remove(i));
                if !self.finalized.iter().any(|m| m.tx_uuid == tx_uuid) {
                    self.finalized.push(existing.unwrap_or(msg));
                }
                self.my_submissions.remove(&tx_uuid);
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

    fn mark_ours(&mut self, msg: &AppMessage) {
        self.my_submissions.insert(msg.tx_uuid, msg.clone());
    }
}

/// Process a channel update event.
///
/// 1. Revert orphaned from state.
/// 2. Apply adopted to state.
/// 3. Iterate our submissions; return any that aren't currently in state and
///    aren't being retried by the SDK (`pending`).
///
/// This pattern handles every reorg shape — including pending we submitted
/// that never made it on chain — because we drive republish decisions from
/// our own submission set, not from SDK-side filtering.
pub fn resolve_conflicts(
    state: &mut InMemoryZoneState,
    orphaned: &[InscriptionInfo],
    adopted: &[InscriptionInfo],
    pending: &[InscriptionInfo],
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

    state
        .my_submissions
        .values()
        .filter(|m| !state.contains(&m.tx_uuid))
        .filter(|m| !pending_uuids.contains(&m.tx_uuid))
        .cloned()
        .collect()
}
