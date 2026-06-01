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
//! The sequencer is the actor: it drives the event stream and exposes sync
//! state (snapshots + watch subscriptions). The handle is the cloneable async
//! command surface for publish / sign / submit / channel-config. Subscribe
//! to whatever state you'll observe from other tasks before spawning the
//! drive task; the receivers stay valid afterwards.
//!
//! ```no_run
//! use futures::StreamExt as _;
//! use lb_zone_sdk::{
//!     CommonHttpClient,
//!     adapter::NodeHttpClient,
//!     sequencer::{Event, ZoneSequencer},
//! };
//! # use lb_core::mantle::ops::channel::ChannelId;
//! # use lb_key_management_system_service::keys::Ed25519Key;
//! # async fn run(channel_id: ChannelId, signing_key: Ed25519Key) {
//! let node = NodeHttpClient::new(CommonHttpClient::new(None), "http://localhost:8080".parse().unwrap());
//! let (sequencer, handle) = ZoneSequencer::init(channel_id, signing_key, node, None);
//!
//! // Subscribe to anything you'll observe from other tasks. The first
//! // `.changed().await` on each receiver returns immediately with the
//! // current value; subsequent calls wait for the next change.
//! let mut ready_rx      = sequencer.subscribe_ready();
//! let mut checkpoint_rx = sequencer.subscribe_checkpoint();
//!
//! // Drive task owns the sequencer:
//! tokio::spawn(async move {
//!     let mut sequencer = sequencer;
//!     let mut events = sequencer.events();
//!     while let Some(event) = events.next().await {
//!         match event {
//!             Event::Readiness { ready }                 => { let _ = ready; }
//!             Event::TxsFinalized { items }              => { let _ = items; }
//!             Event::ChannelUpdate { orphaned, adopted } => { let _ = (orphaned, adopted); }
//!             Event::Published { tx }                    => { let _ = tx; }
//!             Event::Checkpoint { checkpoint }           => { let _ = checkpoint; }
//!             Event::TurnNotification { notification }   => { let _ = notification; }
//!         }
//!     }
//! });
//!
//! // From any other task: async commands via the handle, current state via
//! // the receivers you subscribed to above.
//! ready_rx.changed().await.ok();        // returns immediately with current value
//! if *ready_rx.borrow() {
//!     handle.publish_message(b"hello"[..].try_into().unwrap()).await.ok();
//! }
//! let _last_cp = checkpoint_rx.borrow().clone();
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
