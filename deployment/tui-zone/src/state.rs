use std::{error::Error, fs};

use lb_core::mantle::ops::channel::{ChannelId, MsgId};
use lb_zone_sdk::sequencer::{InscriptionInfo, SequencerChannelView, SequencerCheckpoint};
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

use crate::message::Msg;

const CHECKPOINT_FILE: &str = "sequencer.checkpoint.json";
const CHECKPOINT_KIND: &str = "zone_sequencer_checkpoint";
const CHECKPOINT_VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
struct CheckpointFile {
    version: u8,
    kind: String,
    channel_id: Option<String>,
    checkpoint: String,
}

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
    /// Record adopted inscriptions from the canonical chain.
    fn on_adopted(&mut self, adopted: &[InscriptionInfo]);
    /// Remove our orphaned entry from `published`. Caller is expected to
    /// auto-republish via `sequencer.handle().publish`.
    fn on_orphaned(&mut self, msg_id: &MsgId);
    /// Move finalized inscriptions into the finalized list.
    fn on_finalized(&mut self, inscriptions: &[InscriptionInfo]);

    /// Locally published inscriptions that are not finalized yet.
    fn published(&self) -> &[Msg];
    /// Adopted inscriptions from other writers that are not finalized yet.
    fn adopted(&self) -> &[Msg];
    /// Finalized inscriptions below LIB.
    fn finalized(&self) -> &[Msg];

    /// Persist the sequencer checkpoint for later resume.
    fn save_checkpoint(&mut self, checkpoint: SequencerCheckpoint);
    /// Load the last persisted sequencer checkpoint, if any.
    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint>;
}

/// In-memory implementation of [`ZoneState`].
#[derive(Default)]
pub struct InMemoryZoneState {
    published: Vec<Msg>,
    adopted: Vec<Msg>,
    finalized: Vec<Msg>,
    checkpoint: Option<SequencerCheckpoint>,
    channel_id: Option<ChannelId>,
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
        self.checkpoint = Some(checkpoint.clone());
        if let Some(channel_id) = self.channel_id
            && let Err(error) = save_persisted_checkpoint(&channel_id, &checkpoint)
        {
            error!("failed to save sequencer checkpoint: {error}");
        }
    }

    fn load_checkpoint(&self) -> Option<&SequencerCheckpoint> {
        self.checkpoint.as_ref()
    }
}

/// Load the persisted sequencer checkpoint shared by all TUI zone commands.
pub fn load_persisted_checkpoint(
    channel_id: &ChannelId,
) -> Result<Option<SequencerCheckpoint>, Box<dyn Error + Send + Sync>> {
    load_or_discard_persisted_checkpoint_for_channel(channel_id, true)
}

/// Load the persisted sequencer checkpoint after validating the target channel.
pub fn load_or_discard_persisted_checkpoint_for_channel(
    channel_id: &ChannelId,
    channel_exists: bool,
) -> Result<Option<SequencerCheckpoint>, Box<dyn Error + Send + Sync>> {
    let Ok(bytes) = fs::read(CHECKPOINT_FILE) else {
        return Ok(None);
    };
    let file = serde_json::from_slice::<CheckpointFile>(&bytes)?;
    if file.version != CHECKPOINT_VERSION {
        return Err(format!(
            "unsupported {CHECKPOINT_KIND} version {}; expected {CHECKPOINT_VERSION}",
            file.version
        )
        .into());
    }
    if file.kind != CHECKPOINT_KIND {
        return Err(format!(
            "unsupported checkpoint kind '{}'; expected '{CHECKPOINT_KIND}'",
            file.kind
        )
        .into());
    }
    let expected_channel_id = hex::encode(channel_id.as_ref());
    let checkpoint_channel_id = file.channel_id.ok_or_else(|| {
        format!("checkpoint '{CHECKPOINT_FILE}' is missing channel_id; remove it or recreate it")
    })?;
    if checkpoint_channel_id != expected_channel_id {
        discard_stale_checkpoint(&format!(
            "checkpoint channel_id {checkpoint_channel_id} does not match requested channel_id {expected_channel_id}"
        ))?;
        return Ok(None);
    }
    if !channel_exists {
        discard_stale_checkpoint(&format!(
            "checkpoint channel_id {expected_channel_id} was accepted by file validation, but the node has no channel state for it"
        ))?;
        return Ok(None);
    }
    Ok(Some(bincode::deserialize(&hex::decode(file.checkpoint)?)?))
}

fn discard_stale_checkpoint(reason: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    warn!("discarding stale sequencer checkpoint '{CHECKPOINT_FILE}': {reason}");
    fs::remove_file(CHECKPOINT_FILE)?;
    Ok(())
}

/// Persist the sequencer checkpoint shared by all TUI zone commands.
pub fn save_persisted_checkpoint(
    channel_id: &ChannelId,
    checkpoint: &SequencerCheckpoint,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let file = CheckpointFile {
        version: CHECKPOINT_VERSION,
        kind: CHECKPOINT_KIND.to_owned(),
        channel_id: Some(hex::encode(channel_id.as_ref())),
        checkpoint: hex::encode(bincode::serialize(checkpoint)?),
    };
    fs::write(CHECKPOINT_FILE, serde_json::to_vec_pretty(&file)?)?;
    Ok(())
}

impl InMemoryZoneState {
    /// Create in-memory TUI state with a channel-validated runtime checkpoint.
    pub fn for_channel(
        channel_id: ChannelId,
        channel_exists: bool,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            published: Vec::new(),
            adopted: Vec::new(),
            finalized: Vec::new(),
            checkpoint: load_or_discard_persisted_checkpoint_for_channel(
                &channel_id,
                channel_exists,
            )?,
            channel_id: Some(channel_id),
            channel_view: None,
        })
    }

    /// Store the latest sequencer channel view for rendering.
    pub fn set_channel_view(&mut self, channel_view: SequencerChannelView) {
        self.channel_view = Some(channel_view);
    }

    /// Return the latest sequencer channel view, if one has been observed.
    #[must_use]
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
