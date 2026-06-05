use std::time::{Duration, SystemTime};

use lb_common_http_client::Slot;
use lb_core::{
    crypto::Hash,
    header::HeaderId,
    mantle::{
        SignedMantleTx, Value,
        channel::ChannelState,
        ledger::{Inputs, Outputs},
        ops::channel::{
            ChannelId, MsgId, deposit::Metadata, inscribe::Inscription, withdraw::ChannelWithdrawOp,
        },
        tx::TxHash,
    },
};

const DEFAULT_RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_PUBLISH_CHANNEL_CAPACITY: usize = 256;

/// Inscription identifier.
pub type InscriptionId = TxHash;

/// Checkpoint for stop/resume functionality.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SequencerCheckpoint {
    /// Last message ID for chain continuity.
    pub last_msg_id: MsgId,
    /// Pending transactions to restore.
    pub pending_txs: Vec<(TxHash, SignedMantleTx)>,
    /// Last known LIB.
    pub lib: HeaderId,
    /// Last known LIB slot (for backfill range queries).
    pub lib_slot: Slot,
}

/// Result of a publish operation.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// The inscription ID (transaction hash).
    pub inscription_id: InscriptionId,
}

/// One withdraw to bundle atomically with an inscription.
///
/// The SDK fills `channel_id` and `withdraw_nonce` from internal state.
/// The caller only specifies the outputs (recipients + amounts).
#[derive(Debug, Clone)]
pub struct WithdrawArg {
    pub outputs: Outputs,
}

/// A pending tx that has been orphaned by a chain update.
///
/// The consumer republishes by calling the same SDK method they used
/// originally with the data carried inside the variant:
/// - [`OrphanedTx::Inscription`] → [`super::SequencerHandle::publish_message`]
///   with `info.payload`
/// - [`OrphanedTx::AtomicWithdraw`] →
///   [`super::SequencerHandle::publish_atomic_withdraw`] with
///   `info.inscription.payload` and `WithdrawArg`s reconstructed from
///   `info.withdraws[i].op.outputs`. The SDK fills fresh `parent_msg` and
///   current `withdraw_nonce` internally on each publish.
#[derive(Debug, Clone)]
pub enum OrphanedTx {
    Inscription(InscriptionInfo),
    AtomicWithdraw(AtomicWithdrawInfo),
}

/// Configuration for the zone sequencer.
#[derive(Clone)]
pub struct SequencerConfig {
    pub resubmit_interval: Duration,
    pub reconnect_delay: Duration,
    pub publish_channel_capacity: usize,
    pub slot_duration: Duration,
    pub chain_start_time: Option<SystemTime>,
    pub min_slots_remaining_in_turn: u64,
    pub max_pending_publish_depth: usize,
}

impl Default for SequencerConfig {
    fn default() -> Self {
        Self {
            resubmit_interval: DEFAULT_RESUBMIT_INTERVAL,
            reconnect_delay: DEFAULT_RECONNECT_DELAY,
            publish_channel_capacity: DEFAULT_PUBLISH_CHANNEL_CAPACITY,
            slot_duration: Duration::from_secs(1),
            chain_start_time: None,
            min_slots_remaining_in_turn: 1,
            max_pending_publish_depth: 10,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SequencerChannelView {
    pub channel_id: ChannelId,
    pub channel: Option<ChannelState>,
    pub current_slot: Slot,
    pub own_key_index: Option<u16>,
    pub authorized_key_index: Option<u16>,
    pub our_turn_to_write: bool,
    pub tip_message: MsgId,
    pub pending_publish_txs: usize,
    pub queued_messages: usize,
    pub turn_to_write_slots: Option<u32>,
    pub posting_timeout_slots: Option<u32>,
    pub accredited_key_count: Option<usize>,
}

impl SequencerChannelView {
    pub(super) const fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            channel: None,
            current_slot: Slot::genesis(),
            own_key_index: None,
            authorized_key_index: None,
            our_turn_to_write: false,
            tip_message: MsgId::root(),
            pending_publish_txs: 0,
            queued_messages: 0,
            turn_to_write_slots: None,
            posting_timeout_slots: None,
            accredited_key_count: None,
        }
    }
}

/// Sequencer errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sequencer unavailable: {reason}")]
    Unavailable { reason: &'static str },
    #[error("network error: {0}")]
    Network(String),
}

/// Events emitted by the sequencer.
#[derive(Debug, Clone)]
pub enum Event {
    /// Transactions finalized (at or below LIB), in chain-execution order.
    ///
    /// Emitted in both regimes with identical semantics — consumers write a
    /// single apply loop:
    /// - during cold-start catch-up, multiple `TxsFinalized` events stream in
    ///   (one per backfill batch) before [`Event::Readiness`] with `ready:
    ///   true`
    /// - during live operation, one `TxsFinalized` is emitted whenever LIB
    ///   advances and brings new finalized channel txs
    ///
    /// `items` is one [`FinalizedTx`] per finalized Mantle tx that touched
    /// our channel, in block then tx order. Each [`FinalizedTx`] contains its
    /// channel-relevant ops in on-chain execution order:
    /// - inscriptions (ours or others') → [`FinalizedOp::Inscription`]
    /// - deposits enriched with `amount` from the chain events API →
    ///   [`FinalizedOp::Deposit`]
    /// - withdraws — standalone or bundled with an inscription in the same tx →
    ///   [`FinalizedOp::Withdraw`]
    ///
    /// Atomicity is structural: ops sharing a parent [`FinalizedTx`] were
    /// applied as one Mantle tx.
    ///
    /// Consumers can filter to "ours" via [`Event::Published`] — every tx
    /// this sequencer submits has its hash surfaced there, so the consumer
    /// can match against `items[i].tx_hash`.
    TxsFinalized { items: Vec<FinalizedTx> },
    /// Channel state changed.
    ///
    /// Emitted when at least one of `orphaned` or `adopted` is non-empty.
    /// `safe → pending` transitions whose original signed tx is still valid
    /// (parent unchanged on the new branch) are not surfaced — the SDK
    /// keeps retrying them internally.
    ///
    /// `orphaned` contains only items the SDK has given up on: our own
    /// pending whose original signed tx is permanently invalid because a
    /// competing inscription claimed the parent slot (or because the parent
    /// is now off the canonical chain transitively). These need a user
    /// decision — re-creation requires your signing key.
    ///
    /// `adopted` is the block-delta of inscriptions newly on the canonical
    /// branch, filtered to **exclude items that originated from this
    /// sequencer instance** (matched by `this_msg` against the internal
    /// outbox). Consumers learn about their own publishes via
    /// `Event::Published` (optimistic apply pattern) — those don't need to
    /// be re-surfaced here. The outbox-based filter works correctly even
    /// when multiple sequencer instances share a signing key: each
    /// instance's outbox only contains what it itself submitted.
    ///
    /// Consumer pattern:
    /// 1. On `Event::Published`: optimistically apply your own inscription to
    ///    local state.
    /// 2. On `ChannelUpdate`: apply `adopted` (others' new inscriptions) to
    ///    local state, revert `orphaned` (yours that can no longer land).
    /// 3. For each entry in `orphaned`, decide whether to republish (with a
    ///    fresh parent — SDK handles parent selection).
    ChannelUpdate {
        /// Our pending whose original signed tx is permanently invalid
        /// (parent slot claimed by something in `adopted`, or parent
        /// transitively off canonical).
        ///
        /// For [`OrphanedTx::Inscription`] entries, the consumer republishes
        /// via [`super::SequencerHandle::publish_message`]. For
        /// [`OrphanedTx::AtomicWithdraw`] entries, the consumer republishes
        /// via [`super::SequencerHandle::publish_atomic_withdraw`] with the
        /// original payload and reconstructed [`WithdrawArg`]s from the
        /// bundle's `withdraws`. The SDK fills fresh `parent_msg` and current
        /// `withdraw_nonce` internally on each publish.
        orphaned: Vec<OrphanedTx>,
        /// Others' inscriptions newly on the canonical branch (block-delta,
        /// excluding entries this instance submitted — matched by `this_msg`
        /// against the internal outbox. See `Event::Published` for our own).
        adopted: Vec<InscriptionInfo>,
    },
    /// Sequencer readiness changed.
    ///
    /// `ready: true` is emitted on the up edge — connected, backfill complete,
    /// ready to accept publishes. `ready: false` is emitted on the down edge —
    /// disconnect or transient processing failure dropped the stream; the SDK
    /// is reconnecting. Consumers driving the event loop wait for the next
    /// `Readiness { ready: true }` to resume submitting publishes.
    Readiness { ready: bool },
    /// A tx (plain inscription or atomic-withdraw bundle) was created and
    /// submitted to the network.
    ///
    /// The inner [`PublishedTx`] variant tells the consumer
    /// whether this came from [`super::SequencerHandle::publish_message`] or
    /// [`super::SequencerHandle::publish_atomic_withdraw`]. `this_msg` on the
    /// inscription is the lineage key for correlating later
    /// `ChannelUpdate.orphaned`/`adopted` and `TxsFinalized.items`
    /// entries back to the originating publish call.
    Published { tx: Box<PublishedTx> },
    /// The SDK's checkpoint has advanced.
    ///
    /// Emitted after every backfill batch and after every live block,
    /// following any block-derived events for that block/batch
    /// ([`Event::TxsFinalized`], [`Event::ChannelUpdate`]).
    Checkpoint { checkpoint: SequencerCheckpoint },
    /// Turn-to-write status update for this sequencer.
    ///
    /// Emitted on the same change boundary as the `turn_to_write` watch
    /// channel (excluding `current_slot`-only updates).
    TurnNotification { notification: TurnNotification },
}

/// Information about whose turn it is to post and the current posting
/// timeframe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnNotification {
    /// True if it's currently our turn to write.
    pub our_turn_to_write: bool,
    /// The current turn-to-write slot starting slot.
    pub starting_slot: Option<u64>,
    /// The slot at which the current turn-to-write ends, if known. This is not
    /// guaranteed to be known, as it depends on the channel config and
    /// current slot, which may not be available at the time of
    /// notification.
    pub ends_at_slot: Option<u64>,
    /// The number of slots in the current turn-to-write timeframe, if known.
    /// This is not guaranteed to be known, as it depends on the channel
    /// config and current slot, which may not be available at the time of
    /// notification.
    pub turn_to_write_slots: Option<u32>,
    /// The current slot at the time of notification, if known. This is not
    /// guaranteed to be known, as it depends on the channel config and
    /// current slot, which may not be available at the time
    /// of notification.
    pub current_slot: Option<u64>,
}

/// Channel inscription observed in an L1 block.
#[derive(Debug, Clone)]
pub struct InscriptionInfo {
    /// The transaction hash containing this inscription.
    pub tx_hash: TxHash,
    /// The parent message ID this inscription chains from.
    pub parent_msg: MsgId,
    /// The message ID of this inscription.
    pub this_msg: MsgId,
    /// The opaque inscription payload.
    pub payload: Inscription,
}

/// A channel withdraw observed on chain or bundled in a pending atomic tx.
#[derive(Debug, Clone)]
pub struct WithdrawInfo {
    /// Transaction hash that contained this withdraw op. For bundled
    /// withdraws inside [`AtomicWithdrawInfo`] this equals the bundle's
    /// `tx_hash`; for standalone withdraws surfaced via
    /// [`FinalizedOp::Withdraw`] this is the source tx.
    pub tx_hash: TxHash,
    /// The withdraw op (`channel_id`, outputs, `withdraw_nonce`).
    pub op: ChannelWithdrawOp,
}

/// An inscription bundled atomically with one or more withdraws in a single
/// `MantleTx`. All ops in this tx adopt/orphan/finalize as a unit.
#[derive(Debug, Clone)]
pub struct AtomicWithdrawInfo {
    /// Transaction hash of the bundled `MantleTx`.
    pub tx_hash: TxHash,
    /// The inscription op carried by the bundle.
    pub inscription: InscriptionInfo,
    /// The withdraw ops carried by the bundle, in tx order.
    pub withdraws: Vec<WithdrawInfo>,
}

/// A channel deposit observed in a finalized L1 block. Sequencers do not
/// publish deposits — these are pure observations enriched with the deposit
/// `amount` from the chain events API.
#[derive(Debug, Clone)]
pub struct DepositInfo {
    /// The transaction hash containing this deposit op.
    pub tx_hash: TxHash,
    /// The `op_id` of the deposit op (stable identity within the tx).
    pub op_id: Hash,
    /// Target channel.
    pub channel_id: ChannelId,
    /// Notes consumed by the deposit (spent-once at the UTXO layer).
    pub inputs: Inputs,
    /// Total value deposited, sourced from the block's events.
    pub amount: Value,
    /// Opaque metadata associated with this deposit.
    pub metadata: Metadata,
}

/// A tx surfaced by the SDK in event payloads.
///
/// Either our own publish (inscription / atomic withdraw bundle), or an
/// observed deposit from a finalized L1 block. Used for
/// adopted/published/finalized observations.
#[derive(Debug, Clone)]
pub enum PublishedTx {
    /// A plain inscription published via `publish_message`.
    Inscription(InscriptionInfo),
    /// A bundled inscription+withdraw(s) published via
    /// `publish_atomic_withdraw`.
    AtomicWithdraw(AtomicWithdrawInfo),
}

impl PublishedTx {
    /// The tx hash for this entry.
    #[must_use]
    pub const fn tx_hash(&self) -> TxHash {
        match self {
            Self::Inscription(i) => i.tx_hash,
            Self::AtomicWithdraw(a) => a.tx_hash,
        }
    }

    /// The inscription info for this entry.
    #[must_use]
    pub const fn inscription(&self) -> &InscriptionInfo {
        match self {
            Self::Inscription(i) => i,
            Self::AtomicWithdraw(a) => &a.inscription,
        }
    }
}

/// A finalized Mantle tx that touched our channel.
///
/// Carries the channel-relevant ops in on-chain execution order. Atomicity
/// is structural: every [`FinalizedOp`] inside the same [`FinalizedTx`]
/// succeeded or failed together on chain.
#[derive(Debug, Clone)]
pub struct FinalizedTx {
    /// Transaction hash of the Mantle tx.
    pub tx_hash: TxHash,
    /// Channel-relevant ops in on-chain execution order. A tx with a
    /// deposit and an inscription emits both, deposit-first.
    pub ops: Vec<FinalizedOp>,
}

/// A single channel-relevant op observed in a finalized block. Surfaced as a
/// member of [`FinalizedTx::ops`].
#[derive(Debug, Clone)]
pub enum FinalizedOp {
    /// Any inscription on the channel — ours or another sequencer's.
    Inscription(InscriptionInfo),
    /// A finalized L1 deposit on the channel, with `amount` populated from
    /// the chain events API.
    Deposit(DepositInfo),
    /// A withdraw op on the channel — standalone or part of an atomic
    /// inscription+withdraw bundle (the bundling is implicit via the parent
    /// [`FinalizedTx::tx_hash`]).
    Withdraw(WithdrawInfo),
}
