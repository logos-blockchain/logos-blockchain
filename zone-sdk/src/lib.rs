//! Logos blockchain zone SDK.
//!
//! A Rust client for working with a Logos channel, built around the
//! [`sequencer`] subsystem: drive your own sequencer that publishes
//! inscriptions to a channel, tracks pending/finalized state, and surfaces
//! chain events.
//!
//! There is no separate read-only client: to only observe a channel, run a
//! [`sequencer::ZoneSequencer`] with a signing key that is **not** in the
//! channel's accredited keys. Inscription posting is gated on holding the
//! round-robin turn, so such a sequencer never publishes or reposts anything —
//! while still delivering finalized txs, channel updates and the
//! non-finalized channel view through its events. Before emitting
//! [`sequencer::Event::Ready`], the instance backfills the finalized history
//! it has not seen yet: from genesis on a cold start, or from the restored
//! [`sequencer::SequencerCheckpoint`]'s slot when resuming. Construction
//! still requires a [`sequencer::FundingConfig`]; for a read-only instance
//! pass a placeholder — funding is only exercised by publish-type calls,
//! which such a sequencer never makes.
//!
//! The sequencer talks to a Logos node through the [`adapter`] module's
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
//!     sequencer::{Event, FundingConfig, ZoneSequencer},
//! };
//! # use lb_core::mantle::ops::channel::ChannelId;
//! # use lb_key_management_system_service::keys::Ed25519Key;
//! # async fn run(channel_id: ChannelId, signing_key: Ed25519Key, funding: FundingConfig) {
//! let node = NodeHttpClient::new(CommonHttpClient::new(None), "http://localhost:8080".parse().unwrap());
//! // `funding.funding_pk` is a wallet key the connected node controls (its
//! // configured funding wallet); the node builds and proves the fee transfer.
//! let mut sequencer = ZoneSequencer::init(channel_id, signing_key, node, funding, None);
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
pub mod sequencer;
#[cfg(test)]
mod test_support;

/// Types appearing in the [`adapter::Node`] trait's method signatures and in
/// [`ZoneMessage`], re-exported from the crates that define them.
///
/// An external backend implementation only needs to depend on this crate:
/// everything required to write an `impl Node` — request/response bodies,
/// stream item types, ids, and the error type — is importable from here. The
/// stream alias is [`adapter::BoxStream`]; the message types themselves
/// ([`ZoneMessage`], [`ZoneBlock`], [`Deposit`], [`Withdraw`]) live at the
/// crate root.
pub mod node_types {
    pub use lb_common_http_client::{
        ApiBlock, BlockInfo, ChainServiceInfo, Error, Events, ProcessedBlockEvent, Slot, TimeInfo,
    };
    pub use lb_core::{
        crypto::Hash,
        events::DepositRecreatedNotes,
        header::HeaderId,
        mantle::{
            SignedOps, TxHash, Value,
            channel::ChannelState,
            ledger::Inputs,
            ops::channel::{ChannelId, MsgId, deposit::Metadata, inscribe::Inscription},
            transactions::states::Unverified,
        },
    };
    pub use lb_http_api_common::bodies::wallet::fund::{
        WalletFundRequestBody, WalletFundResponseBody,
    };
}

pub use lb_common_http_client::{CommonHttpClient, Slot};
pub use lb_core::mantle::ops::channel::Ed25519PublicKey;
use lb_core::{
    crypto::Hash,
    events::DepositRecreatedNotes,
    mantle::{
        TxHash, Value,
        ledger::Inputs,
        ops::channel::{MsgId, deposit::Metadata, inscribe::Inscription},
    },
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
    /// The transaction hash containing this deposit op.
    pub tx_hash: TxHash,
    /// The `op_id` of the deposit op: its stable identity within the tx,
    /// matching `DepositInfo::op_id` on the sequencer side.
    pub op_id: Hash,
    /// Notes consumed by the deposit (spent-once at the UTXO layer).
    pub inputs: Inputs,
    /// The channel notes the deposit re-created its inputs as, sourced from
    /// the block's events. These carry new `NoteId`s and are what the channel
    /// now owns.
    pub notes: DepositRecreatedNotes,
    /// Total value deposited, sourced from the block's events.
    pub amount: Value,
    /// Opaque metadata associated with this deposit
    pub metadata: Metadata,
}

/// An withdrawal from a zone channel, included/finalized in Bedrock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withdraw {
    /// The transaction hash containing this withdraw op.
    pub tx_hash: TxHash,
    /// The `op_id` of the withdraw op: its stable identity within the tx.
    pub op_id: Hash,
    /// The channel notes released by the withdrawal
    pub inputs: Inputs,
}

impl Deposit {
    #[must_use]
    pub fn metadata(&self) -> &[u8] {
        &self.metadata
    }
}
