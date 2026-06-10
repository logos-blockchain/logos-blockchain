//! Logos blockchain zone SDK.
//!
//! A Rust client for working with a Logos channel. Two subsystems:
//!
//! - [`sequencer`] — drive your own sequencer that publishes inscriptions to a
//!   channel, tracks pending/finalized state, and surfaces chain events.
//! - [`indexer`] — read-only stream of finalized channel messages, for
//!   consumers that only need to observe.
//!
//! Both subsystems talk to a Logos node through the [`adapter`] module's
//! [`adapter::Node`] trait; an HTTP implementation is provided as
//! [`adapter::NodeHttpClient`].
//!
//! # Quick start (sequencer)
//!
//! The sequencer is owned by a single drive task. State-mutating commands go
//! through a borrowing handle obtained via
//! [`sequencer::ZoneSequencer::handle`]; every publish returns the resulting
//! [`sequencer::SequencerCheckpoint`] inline so the caller can persist outbox +
//! checkpoint atomically. Read-only observers (other tasks) get cloneable watch
//! receivers from the subscribe methods.
//!
//! ```ignore
//! use lb_zone_sdk::{
//!     CommonHttpClient,
//!     adapter::NodeHttpClient,
//!     sequencer::{Event, ZoneSequencer},
//! };
//! # use lb_core::mantle::ops::channel::ChannelId;
//! # use lb_key_management_system_service::keys::Ed25519Key;
//! # async fn run(channel_id: ChannelId, signing_key: Ed25519Key) {
//! let node = NodeHttpClient::new(CommonHttpClient::new(None), "http://localhost:8080".parse().unwrap());
//! let mut sequencer = ZoneSequencer::init(channel_id, signing_key, node, None);
//!
//! // Other tasks observe via cloneable watch receivers (subscribe before
//! // moving the sequencer into its drive task).
//! let _ready_rx      = sequencer.subscribe_ready();
//! let _checkpoint_rx = sequencer.subscribe_checkpoint();
//!
//! loop {
//!     tokio::select! {
//!         Some(ev) = sequencer.next_event() => match ev {
//!             Event::BlocksProcessed { checkpoint, channel_update, finalized } => {
//!                 let _ = (checkpoint, channel_update, finalized);
//!             }
//!             Event::Ready                             => {}
//!             Event::TurnNotification { notification } => { let _ = notification; }
//!         },
//!     }
//! }
//! # }
//! ```
//!
//! See the [`sequencer`] module for the full event vocabulary.

pub mod adapter;
pub mod indexer;
pub mod sequencer;

pub use lb_common_http_client::{CommonHttpClient, Slot};
pub use lb_core::mantle::ops::channel::Ed25519PublicKey;
use lb_core::mantle::{
    Value,
    ledger::{Inputs, Outputs},
    ops::channel::{MsgId, deposit::Metadata, inscribe::Inscription},
};

/// A message from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoneMessage {
    /// A zone block published to a channel
    Block(ZoneBlock),
    /// A deposit operation submitted to a channel
    Deposit(Deposit),
    /// An withdraw operation submitted to a channel
    Withdraw(Withdraw),
}

/// A zone block from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneBlock {
    /// The unique identifier of this inscription.
    pub id: MsgId,
    /// The opaque inscription data.
    pub data: Inscription,
}

/// A deposit from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deposit {
    /// Notes consumed by the deposit. Acts as the natural unique key (notes
    /// are spent-once at the UTXO layer).
    pub inputs: Inputs,
    /// Total value deposited, sourced from the block's events.
    pub amount: Value,
    /// Opaque metadata associated with this deposit
    pub metadata: Metadata,
}

/// An withdrawal from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withdraw {
    /// Amount of the withdrawal
    pub outputs: Outputs,
}

impl Deposit {
    #[must_use]
    pub fn metadata(&self) -> &[u8] {
        &self.metadata
    }
}
