use std::time::Duration;

use lb_common_http_client::Slot;
use lb_core::{
    crypto::Hash,
    header::HeaderId,
    mantle::{
        SignedMantleTx, Transaction as _, Value,
        channel::ChannelState,
        gas::GasCost,
        ledger::{Inputs, Outputs},
        ops::channel::{
            ChannelId, MsgId, deposit::Metadata, inscribe::Inscription, withdraw::ChannelWithdrawOp,
        },
        transactions::TxHash,
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;

const DEFAULT_RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_PUBLISH_CHANNEL_CAPACITY: usize = 256;
const DEFAULT_MAX_LOCAL_TX_TRACKING: usize = 10_000;

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
///
/// `tx` carries the [`PendingTx`] the caller just enqueued — inscriptions or
/// atomic-withdraw bundles. The tx has been accepted into the sequencer's
/// pending set and the post is queued onto the drive loop's in-flight pool;
/// it has not necessarily been delivered to the node yet. Consumers use this
/// to apply the entry to their local state right away — it won't echo back
/// in [`ChannelUpdate::adopted`] on chain extension. On a branch change the
/// full delta is reported, where `this_msg` is the dedup identity.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// The enqueued tx (inscription or atomic withdraw bundle).
    pub tx: PendingTx,
}

impl PublishResult {
    /// The inscription id (transaction hash) of the submitted tx.
    #[must_use]
    pub const fn inscription_id(&self) -> InscriptionId {
        self.tx.tx_hash()
    }
}

/// One withdraw to bundle atomically with an inscription.
///
/// The SDK fills `channel_id` and `withdraw_nonce` from internal state.
/// The caller only specifies the outputs (recipients + amounts).
#[derive(Debug, Clone)]
pub struct WithdrawArg {
    pub outputs: Outputs,
}

/// A tx reported in a [`ChannelUpdate`], in both `adopted` and `orphaned`.
///
/// The variants mirror the submission flows, so an orphaned entry is
/// recovered with the same method that produced it:
/// - [`ChannelUpdateTx::Inscription`] →
///   [`SequencerHandle::publish`](super::SequencerHandle::publish) with
///   `info.payload`
/// - [`ChannelUpdateTx::AtomicWithdraw`] →
///   [`SequencerHandle::publish_atomic_withdraw`](super::SequencerHandle::publish_atomic_withdraw)
///   with `info.inscription.payload` and `WithdrawArg`s reconstructed from
///   `info.withdraws[i].op.outputs`. The SDK fills fresh `parent_msg` and
///   current `withdraw_nonce` internally on each publish.
/// - [`ChannelUpdateTx::Custom`] → the `prepare_tx` + `submit_signed_tx` flow:
///   the SDK cannot demystify the tx, so it hands back the whole
///   [`SignedMantleTx`] and the caller's own logic decides how to parse and
///   whether/how to rebuild it (an orphaned tx cannot be re-posted as-is — its
///   parent slot is consumed).
///   [`channel_inscriptions`](super::channel_inscriptions) extracts the tx's
///   channel inscriptions the way the SDK sees them.
#[derive(Debug, Clone)]
pub enum ChannelUpdateTx {
    /// A published message.
    Inscription(InscriptionInfo),
    /// An atomic inscription+withdraw bundle.
    AtomicWithdraw(AtomicWithdrawInfo),
    /// A tx shape the SDK cannot produce (bundled deposits, multi-inscribe,
    /// other custom-built txs), reported whole as a unit.
    Custom(SignedMantleTx),
}

impl ChannelUpdateTx {
    #[must_use]
    pub fn tx_hash(&self) -> TxHash {
        match self {
            Self::Inscription(i) => i.tx_hash,
            Self::AtomicWithdraw(a) => a.tx_hash,
            Self::Custom(tx) => tx.mantle_tx.hash(),
        }
    }

    /// The inscription carried by this entry; `None` for [`Self::Custom`]
    /// entries, whose content the caller parses itself (see
    /// [`channel_inscriptions`](super::channel_inscriptions)).
    #[must_use]
    pub const fn inscription(&self) -> Option<&InscriptionInfo> {
        match self {
            Self::Inscription(i) => Some(i),
            Self::AtomicWithdraw(a) => Some(&a.inscription),
            Self::Custom(_) => None,
        }
    }
}

/// Configuration for funding transactions from the node's wallet.
///
/// Mirrors the node's SDP wallet config: both values come from the operator's
/// own node configuration (the node sponsors the gas; change returns to
/// `funding_pk`).
#[derive(Clone, Debug)]
pub struct FundingConfig {
    /// The node wallet key that pays transaction fees.
    pub funding_pk: ZkPublicKey,
    /// Hard cap on the fee of a single transaction.
    pub max_tx_fee: GasCost,
}

/// Configuration for the zone sequencer.
#[derive(Clone)]
pub struct SequencerConfig {
    pub resubmit_interval: Duration,
    pub reconnect_delay: Duration,
    pub publish_channel_capacity: usize,
    pub min_slots_remaining_in_turn: u64,
    pub max_pending_publish_depth: usize,
    pub max_local_tx_tracking: usize,
    /// Fund transactions from the node's wallet before signing. `None`
    /// builds fee-less transactions (only valid while gas prices are zero).
    pub funding: Option<FundingConfig>,
}

impl Default for SequencerConfig {
    fn default() -> Self {
        Self {
            resubmit_interval: DEFAULT_RESUBMIT_INTERVAL,
            reconnect_delay: DEFAULT_RECONNECT_DELAY,
            publish_channel_capacity: DEFAULT_PUBLISH_CHANNEL_CAPACITY,
            min_slots_remaining_in_turn: 1,
            max_pending_publish_depth: 10,
            max_local_tx_tracking: DEFAULT_MAX_LOCAL_TX_TRACKING,
            funding: None,
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
///
/// [`Event::BlocksProcessed`] is the state-mutating event — it carries a
/// [`SequencerCheckpoint`] reflecting state after the event is applied, so
/// consumers persist the contained fields plus `checkpoint` in one atomic
/// transaction.
///
/// [`Event::Ready`] and [`Event::TurnNotification`] are lifecycle / turn-
/// status signals; they do not mutate consumer state and do not carry a
/// checkpoint.
///
/// Publishes mutate state synchronously inside the
/// [`SequencerHandle`](super::SequencerHandle) methods that produce them; those
/// methods return the resulting [`SequencerCheckpoint`] inline. There is no
/// separate `Published` event.
#[derive(Debug, Clone)]
pub enum Event {
    /// Fires per ingested block. Carries finalized txs and the non-finalized
    /// channel-tip delta (`channel_update`); either may be empty. Backfill
    /// batches (cold start and reconnect catch-up) emit a single
    /// `BlocksProcessed` per batch with empty `channel_update` — backfill
    /// walks canonical history, so there is no tip delta to report.
    BlocksProcessed {
        checkpoint: SequencerCheckpoint,
        channel_update: ChannelUpdate,
        finalized: Vec<FinalizedTx>,
    },
    /// The sequencer is connected and ready to publish. Emitted once when
    /// cold-start backfill completes, and again after every mid-life
    /// reconnect once a live block confirms the connection.
    ///
    /// With funding configured ([`SequencerConfig::funding`]), publishes
    /// fail fast with [`Error::Unavailable`] while disconnected (funding
    /// needs the node), so the re-emission is the signal to retry. Fee-less
    /// sequencers accept publishes locally throughout a disconnect;
    /// in-memory state stays valid either way, and any tx invalidated by
    /// the catch-up surfaces via [`ChannelUpdate::orphaned`] on the next
    /// `BlocksProcessed` once the stream resumes.
    Ready,
    /// Transaction was accepted by the node post API and is expected to be in
    /// the mempool.
    MempoolPending(TxHash),
    /// Turn-to-write status update for this sequencer.
    ///
    /// Emitted on the same change boundary as the `turn_to_write` watch
    /// channel (excluding `current_slot`-only updates).
    TurnNotification { notification: TurnNotification },
}

/// Tx-hash lifecycle status for a transaction observed by the sequencer.
///
/// This enum tracks the lifecycle of a specific transaction hash, not a
/// publish intent. Republishing after an orphan or any other retry produces a
/// new tx hash and therefore a new lifecycle.
///
/// Typical flows:
/// - Plain success: `AcceptedLocally -> PendingMempool -> OnChain(_) ->
///   Finalized(_)`
/// - Reorg on the same hash: `... -> OnChain(_) -> Orphaned(_) -> OnChain(_) ->
///   Finalized(_)`
/// - Republish after orphan: original hash reaches `Orphaned(_)`; the
///   republished tx starts over at `AcceptedLocally` under its new hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TxStatus {
    /// Accepted by the SDK and tracked locally.
    AcceptedLocally,
    /// Accepted by the node post API and expected to be in the mempool.
    PendingMempool,
    /// Observed in the canonical non-finalized chain.
    OnChain(TxSource),
    /// Previously tracked tx was invalidated on the current canonical branch.
    ///
    /// This is branch-local, not a permanent tombstone: the same hash can
    /// later resurface as [`TxStatus::OnChain`] or [`TxStatus::Finalized`]
    /// after a deeper reorg.
    Orphaned(TxSource),
    /// Observed in finalized chain history.
    Finalized(TxSource),
}

/// Status update for a single transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxStatusUpdate {
    pub tx_hash: TxHash,
    pub status: TxStatus,
}

/// Whether an observed transaction is still attributable to this sequencer
/// runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TxSource {
    /// The tx was accepted locally by this sequencer or restored from its
    /// checkpoint.
    Local,
    /// The tx was observed on chain but is not currently known as local to
    /// this sequencer.
    ///
    /// This includes both genuinely external txs and txs that were previously
    /// local but have since been evicted from the bounded local-tracking set.
    Other,
}

/// How the channel changed across one [`Event::BlocksProcessed`].
///
/// The channel is an ordered chain of inscriptions. It can momentarily fork —
/// competing inscriptions chain off the same parent — and this reports how the
/// canonical chain moved since the last event:
///
/// - `adopted`: inscriptions now on the channel that weren't before — apply
///   them.
/// - `orphaned`: inscriptions that were on the channel (or that you published
///   and were still waiting to land) and no longer are — revert them and treat
///   them as republish candidates.
///
/// Both empty means nothing changed.
///
/// Consumer pattern:
/// 1. On each event, mirror the channel: revert every `orphaned` entry and
///    apply every `adopted` entry.
/// 2. Process the orphans so no useful work is lost — e.g. if your inscriptions
///    carry Zone transactions, return them to your mempool. Reprocessing is
///    idempotent: anything still valid is already pending and no-ops, so only
///    genuinely-dead work is re-sent.
#[derive(Debug, Clone)]
pub struct ChannelUpdate {
    /// Txs removed from the channel: ones that were on chain, plus our
    /// own pending that can no longer finalize because a conflicting
    /// inscription took their place in the chain (a parent double-spend).
    /// Revert from state and treat as republish candidates.
    ///
    /// See [`ChannelUpdateTx`] for how the consumer republishes each
    /// variant.
    pub orphaned: Vec<ChannelUpdateTx>,
    /// Txs added to the channel — every tx that advanced the canonical
    /// channel tip: messages, atomic withdraw bundles or custom txs.
    ///
    /// On a pure extension (`orphaned` empty) this carries only entries the
    /// sequencer wasn't already tracking — its own publishes apply to
    /// consumer state at publish time and don't echo back. On a branch
    /// change the full delta is reported (entries can move between
    /// branches), so consumers dedup by `this_msg` against their own state
    /// there.
    pub adopted: Vec<ChannelUpdateTx>,
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

/// A tx enqueued for posting and surfaced as a publish return value or in
/// orphan payloads.
///
/// Either our own pending publish (inscription / atomic withdraw bundle), or
/// a pending bundle reconstructed on checkpoint resume. Returned from publish
/// methods and carried by [`ChannelUpdateTx`] adjacent contexts. The "pending"
/// framing reflects that the tx has been accepted into local pending state
/// and queued for posting, not that the node has accepted it yet — see
/// [`PublishResult`].
#[derive(Debug, Clone)]
pub enum PendingTx {
    /// A plain inscription published via `publish`.
    Inscription(InscriptionInfo),
    /// A bundled inscription+withdraw(s) published via
    /// `publish_atomic_withdraw`.
    AtomicWithdraw(AtomicWithdrawInfo),
}

impl PendingTx {
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
