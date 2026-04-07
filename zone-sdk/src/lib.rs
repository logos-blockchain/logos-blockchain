pub mod adapter;
pub mod indexer;
pub mod sequencer;
pub mod state;

pub use lb_common_http_client::{CommonHttpClient, Slot};
pub use lb_core::mantle::ops::channel::Ed25519PublicKey;
use lb_core::mantle::{Value, ops::channel::MsgId};

/// A message from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoneMessage {
    /// A zone block published to a channel
    Block(ZoneBlock),
    /// A deposit operation submitted to a channel
    Deposit(Deposit),
}

/// A zone block from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneBlock {
    /// The unique identifier of this inscription.
    pub id: MsgId,
    /// The opaque inscription data.
    pub data: Vec<u8>,
}

/// A deposit from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deposit {
    /// Amount of the deposit
    pub amount: Value,
    /// Opaque metadata associated with this deposit
    metadata: Vec<u8>,
}
