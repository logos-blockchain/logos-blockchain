use std::collections::HashSet;

use lb_key_management_system_service::keys::Ed25519PublicKey;
use lb_zone_sdk::{sequencer::SequencerCheckpoint, state::InscriptionInfo};
use uuid::Uuid;

use crate::message::AppMessage;

/// Trait for zone state management.
///
/// The sequencer surfaces chain events (reorgs, finalization); the application
/// maintains its own view of the world by implementing this trait.
///
/// Ownership of an inscription is determined by comparing the SDK-supplied
/// `signer` field on each `InscriptionInfo` against our own signing key's
/// public key — no app-side tracking is required.
///
/// A production implementation might use a database. This demo uses in-memory
/// vecs.
pub trait ZoneState {
    /// Apply a message to the canonical (unfinalized) state.
    fn apply(&mut self, msg: AppMessage);

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
}

/// Process a channel update event.
///
/// State here represents "messages that have been published" (ours via
/// `Event::Published` optimistic apply, others' via `adopted`) — it does NOT
/// track current canonical membership. Once a message is added it stays;
/// there is no revert. Reorgs and bouncing don't change what was published,
/// only where it lives on chain.
///
/// 1. Apply adopted to state (others' new inscriptions).
/// 2. Iterate orphaned and return entries not in `pending` — those are
///    republish candidates the user must decide on.
pub fn resolve_conflicts(
    state: &mut InMemoryZoneState,
    orphaned: &[InscriptionInfo],
    adopted: &[InscriptionInfo],
    pending: &[InscriptionInfo],
    my_signer: &Ed25519PublicKey,
) -> Vec<AppMessage> {
    // SDK contract: every entry in `orphaned` is one of our own pending items.
    for inv in orphaned {
        debug_assert_eq!(
            &inv.signer, my_signer,
            "orphaned entry signer mismatch - SDK should only surface our own pending"
        );
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

    orphaned
        .iter()
        .filter_map(|inv| AppMessage::from_bytes(&inv.payload))
        .filter(|m| !pending_uuids.contains(&m.tx_uuid))
        .collect()
}
