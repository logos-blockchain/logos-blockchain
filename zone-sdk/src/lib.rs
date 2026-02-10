pub mod indexer;
pub mod sequencer;
pub mod state;

use lb_core::mantle::ops::channel::MsgId;

/// A zone block — opaque data published to / read from a channel.
pub struct ZoneBlock {
    /// The unique identifier of this inscription.
    pub id: MsgId,
    /// The opaque inscription data.
    pub data: Vec<u8>,
}
