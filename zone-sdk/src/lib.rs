pub mod indexer;
pub mod sequencer;
pub mod state;

/// A zone block — opaque data published to / read from a channel.
pub struct ZoneBlock {
    pub data: Vec<u8>,
}
