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
//! The sequencer is owned by a single drive task. Two command surfaces:
//!
//! - [`sequencer::SequencerHandle`] (sync, drive-loop only) — obtained via
//!   [`sequencer::ZoneSequencer::handle`]. Borrows the sequencer mutably so it
//!   can only be used from the drive task. Every state-mutating method returns
//!   the resulting [`sequencer::SequencerCheckpoint`] inline so the caller can
//!   persist outbox + checkpoint atomically.
//! - [`sequencer::SequencerClient`] (async, cross-task) — obtained via
//!   [`sequencer::ZoneSequencer::client`]. Cheap-to-clone; routes publish/admin
//!   commands through the actor's request channel and vends subscription
//!   receivers off cloned senders. Useful for service-style integrations where
//!   publish calls originate from tasks other than the drive loop. The drive
//!   loop must still be polled for client publish awaits to resolve.
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
//! // Cross-task surface: clone and move into any task. Carries both
//! // publish/admin methods and subscription receivers.
//! let client = sequencer.client();
//! let _ready_rx      = client.subscribe_ready();
//! let _checkpoint_rx = client.subscribe_checkpoint();
//!
//! loop {
//!     match sequencer.next_event().await {
//!         Event::BlocksProcessed { checkpoint, channel_update, finalized } => {
//!             let _ = (checkpoint, channel_update, finalized);
//!         }
//!         Event::Ready                             => {}
//!         Event::TurnNotification { notification } => { let _ = notification; }
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
