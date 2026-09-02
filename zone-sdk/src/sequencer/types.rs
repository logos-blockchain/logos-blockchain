use std::time::Duration;

use lb_common_http_client::Slot;
use lb_core::{
    crypto::Hash,
    events::DepositRecreatedNotes,
    header::HeaderId,
    mantle::{
        SignedOps, Value,
        channel::ChannelState,
        gas::GasCost,
        ledger::{Inputs, NoteId, Outputs, verification_mode::StandardMode},
        ops::{
            OpProof,
            channel::{
                ChannelId, MsgId, channel_transfer::ChannelTransferOp, deposit::Metadata,
                inscribe::Inscription, withdraw::ChannelWithdrawOp,
            },
        },
        traits::Hashable as _,
        transactions::{Ops, TxHash, states::Unverified},
    },
};
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};

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
    pub pending_txs: Vec<(TxHash, SignedOps<Unverified, StandardMode>)>,
    /// Last known LIB.
    pub lib: HeaderId,
    /// Last known LIB slot (for backfill range queries).
    pub lib_slot: Slot,
    /// Finalized channel notes as of `lib`. Defaults to empty when restoring
    /// a checkpoint written before channel-note tracking existed — such
    /// restores miss notes finalized before the checkpoint; a cold start
    /// rebuilds the full set.
    #[serde(default)]
    pub channel_notes: Vec<ChannelNote>,
    /// The finalized config-lineage tip as of `lib`. Restored so
    /// `config_tip_at` keeps resolving the right config parent after a warm
    /// restart, once the config's block has been pruned below LIB. Defaults
    /// to [`MsgId::root`] for checkpoints written before this field existed
    /// (matching the old reset-to-root behavior).
    #[serde(default = "MsgId::root")]
    pub finalized_config: MsgId,
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

/// A channel-config transaction built and funded by the SDK, handed back for
/// external multi-sig signing before submission.
///
/// Produced by
/// [`prepare_channel_config`](super::SequencerClient::prepare_channel_config)
/// and consumed by
/// [`submit_channel_config`](super::SequencerClient::submit_channel_config).
/// The caller collects a signature from each key holder over
/// [`Self::sign_payload`], assembles an ascending-by-index
/// `Vec<IndexedSignature>`, and submits it alongside the (unchanged) prepared
/// value. `tx` and `transfer_proof` are opaque to the caller — they carry the
/// funded transaction and its fee-transfer proof straight back into
/// submission.
#[derive(Debug, Clone)]
pub struct PreparedChannelConfig {
    pub(crate) tx: Ops,
    pub(crate) transfer_proof: Option<OpProof>,
    /// The exact bytes each accredited key must sign (the funded tx hash's
    /// signing bytes).
    pub sign_payload: Vec<u8>,
    /// The channel's current accredited keys, in index order. Each collected
    /// signature must be indexed by this key's position here. Empty for an
    /// unclaimed channel, which needs no signatures.
    pub accredited_keys: Vec<Ed25519PublicKey>,
    /// The channel's current `configuration_threshold` — how many of the
    /// `accredited_keys` must sign for the config to be valid. `0` for an
    /// unclaimed channel.
    pub signing_threshold: u16,
}

/// One withdraw to bundle atomically with an inscription.
///
/// The SDK fills `channel_id` from internal state.
/// The caller only specifies the outputs (recipients + amounts).
#[derive(Debug, Clone)]
pub struct WithdrawArg {
    pub outputs: Outputs,
}

/// Which channel notes fund the transfer half of an atomic withdraw.
///
/// The withdraw first transfers channel notes to the recipient keys, so the
/// bundle has to consume enough channel notes to cover the withdrawn amount.
/// [`Self::Auto`] lets the SDK pick them — covering with the newest notes first
/// (spending recent notes and preserving the older, matured positions) and
/// sweeping dust into the remaining input slots to keep the wallet compact.
/// [`Self::Explicit`] pins an exact input set the caller has chosen — e.g. to
/// apply its own dust/age policy — which the SDK validates against the tracked
/// note set. A caller sources those ids from
/// [`ZoneSequencer::channel_wallet`](super::ZoneSequencer::channel_wallet),
/// which lists the tracked [`ChannelNote`]s (id, value, pk, slot).
///
/// On orphan recovery the input choice need not be reproduced: republish with
/// `Auto` (reselects the right notes for the new branch) or a fresh `Explicit`
/// set re-derived from the current `channel_wallet()` view. The rebuilt bundle
/// re-anchors on the current tip, so the orphaned one can no longer land — the
/// input set carries no identity of its own.
#[derive(Debug, Clone)]
pub enum WithdrawInputs {
    /// Let the SDK select covering notes from the tracked note set.
    Auto,
    /// Use exactly these notes, in this order — typically chosen from
    /// [`ZoneSequencer::channel_wallet`](super::ZoneSequencer::channel_wallet).
    Explicit(Vec<NoteId>),
}

/// A tx reported in a [`ChannelUpdate`]'s `adopted`/`orphaned`.
///
/// Variants mirror the submission flows, so an orphaned entry is recovered with
/// the method that produced it (the SDK fills a fresh `parent_msg` each time):
/// - [`ChannelUpdateTx::Inscription`] →
///   [`publish`](super::SequencerHandle::publish) with `info.payload`.
/// - [`ChannelUpdateTx::AtomicWithdraw`] →
///   [`publish_atomic_withdraw`](super::SequencerHandle::publish_atomic_withdraw)
///   with `info.inscription.payload` and `WithdrawArg { outputs: info.outputs
///   }`.
/// - [`ChannelUpdateTx::PinDeposit`] →
///   [`publish_pin_deposit`](super::SequencerHandle::publish_pin_deposit) with
///   `info.inscription.payload` and `info.consumed_notes`.
/// - [`ChannelUpdateTx::Config`] / [`ChannelUpdateTx::Custom`] →
///   caller-recovered; the SDK cannot re-sign a multi-sig config or rebuild a
///   custom tx, so it never auto-resubmits them.
#[derive(Debug, Clone)]
pub enum ChannelUpdateTx {
    /// A published message.
    Inscription(InscriptionInfo),
    /// An atomic inscription+withdraw bundle.
    AtomicWithdraw(AtomicWithdrawInfo),
    /// An atomic inscription+transfer bundle pinning an observed deposit.
    PinDeposit(PinDepositInfo),
    /// A config-only tx (a single `ChannelConfig` op) on the config lineage.
    /// Caller-recovered, like [`Self::Custom`] — never auto-resubmitted.
    Config(SignedOps<Unverified, StandardMode>),
    /// A tx shape the SDK cannot produce (bundled deposits, multi-inscribe,
    /// other custom-built txs), reported whole as a unit.
    Custom(SignedOps<Unverified, StandardMode>),
}

impl ChannelUpdateTx {
    #[must_use]
    pub fn tx_hash(&self) -> TxHash {
        match self {
            Self::Inscription(i) => i.tx_hash,
            Self::AtomicWithdraw(a) => a.tx_hash,
            Self::PinDeposit(a) => a.tx_hash,
            Self::Config(tx) | Self::Custom(tx) => tx.hash(),
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
            Self::PinDeposit(a) => Some(&a.inscription),
            Self::Config(_) | Self::Custom(_) => None,
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
    /// Where an atomic withdraw's transfer change (the overpay beyond the
    /// withdrawn amount, plus any swept dust) is sent. `None` routes change
    /// back to [`Self::funding_pk`]; set it to keep the funding key from
    /// doubling as an implicit custody destination for channel change.
    pub change_pk: Option<ZkPublicKey>,
    /// Absolute hard cap on the fee of a single funded transaction.
    pub max_tx_fee: GasCost,
    /// Percentage of the final mandatory fee reserved as a priority reserve
    /// when funding a transaction. The mandatory fee is execution plus
    /// storage cost, and only the unused reserve becomes an effective tip.
    /// The 12% default is a practical reserve intended to absorb normal fee
    /// movement, including approximately one storage-market epoch increase
    /// at normal price levels. It is not a protocol guarantee at very low
    /// prices or when execution fees also rise materially: storage prices use
    /// integer arithmetic, so a low price can jump proportionally more (for
    /// example, 1 to 2). [`Self::max_tx_fee`] caps the total.
    pub priority_fee_percent: u64,
}

impl FundingConfig {
    /// Default percentage reserve for normal fee movement.
    pub const DEFAULT_PRIORITY_FEE_PERCENT: Value = 12;
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
    /// Fund transactions from the node's wallet before signing.
    pub funding: FundingConfig,
}

impl SequencerConfig {
    /// Config with default values for everything but `funding`.
    #[must_use]
    pub const fn new(funding: FundingConfig) -> Self {
        Self {
            resubmit_interval: DEFAULT_RESUBMIT_INTERVAL,
            reconnect_delay: DEFAULT_RECONNECT_DELAY,
            publish_channel_capacity: DEFAULT_PUBLISH_CHANNEL_CAPACITY,
            min_slots_remaining_in_turn: 1,
            max_pending_publish_depth: 10,
            max_local_tx_tracking: DEFAULT_MAX_LOCAL_TX_TRACKING,
            funding,
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
    /// Channel deposits observed in this block. Surfaced non-finalized so a
    /// consumer can pin a deposit without waiting for finalization, via
    /// [`publish_pin_deposit`](super::SequencerHandle::publish_pin_deposit).
    ///
    /// Reconcile against branch state, don't fire once: a branch change can
    /// re-surface the same deposit or reorg it out. Publish an inscription for
    /// a deposit only while it is on the current branch and none consuming
    /// it is already in flight, and republish if yours is reported in
    /// `orphaned`. The bundle's transfer consumes the deposited note, so it
    /// can only land where the deposit is.
    pub adopted_deposits: Vec<DepositInfo>,
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
    /// The accredited key that signed this inscription (the message author).
    /// `None` for a channel-config entry, which is authorized by a threshold of
    /// keys rather than a single signer and carries no author.
    pub signer: Option<Ed25519PublicKey>,
}

/// A channel withdraw observed on chain or bundled in a pending atomic tx.
#[derive(Debug, Clone)]
pub struct WithdrawInfo {
    /// Transaction hash that contained this withdraw op. For bundled
    /// withdraws inside [`AtomicWithdrawInfo`] this equals the bundle's
    /// `tx_hash`; for standalone withdraws surfaced via
    /// [`FinalizedOp::Withdraw`] this is the source tx.
    pub tx_hash: TxHash,
    /// The withdraw op (`channel_id`, inputs).
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
    /// The recipient notes the bundle releases (the transfer outputs the
    /// withdraw consumes, change excluded). Enough to re-issue the bundle from
    /// its orphan report as a single [`WithdrawArg`] — the note IDs in
    /// `withdraws[i].op.inputs` alone can't recover the recipient value + key.
    pub outputs: Outputs,
}

/// An inscription bundled atomically with a channel transfer that consumes an
/// observed deposited note, in a single `MantleTx`. The transfer makes the
/// inscription conditional on the deposit being on chain: if the deposit is not
/// present the consumed note does not exist, the transfer fails, and the whole
/// tx (inscription included) fails. All ops adopt/orphan/finalize as a unit.
#[derive(Debug, Clone)]
pub struct PinDepositInfo {
    /// Transaction hash of the bundled `MantleTx`.
    pub tx_hash: TxHash,
    /// The inscription op carried by the bundle.
    pub inscription: InscriptionInfo,
    /// The channel notes the bundled transfer consumes (the deposit being
    /// pinned). Recovering an orphaned bundle re-supplies these.
    pub consumed_notes: Inputs,
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
    /// The channel notes the deposit re-created its inputs as, sourced from
    /// the block's events. These carry new `NoteId`s and are what the channel
    /// now owns.
    pub notes: DepositRecreatedNotes,
    /// Total value deposited, sourced from the block's events.
    pub amount: Value,
    /// Opaque metadata associated with this deposit.
    pub metadata: Metadata,
}

/// A channel transfer observed on chain: channel notes consumed and
/// re-created under new keys/denominations by the channel committee.
#[derive(Debug, Clone)]
pub struct ChannelTransferInfo {
    /// Transaction hash that contained this transfer op.
    pub tx_hash: TxHash,
    /// The transfer op (`channel_id`, inputs, outputs).
    pub op: ChannelTransferOp,
}

/// A note owned by the channel, as reconstructed from block data.
///
/// Every note carries its exact `value`, owning `pk`, and creation `slot`:
/// deposits publish value and pk per note in the deposit event
/// (`TxEventPayload::Deposit`), `ChannelTransfer` outputs carry full notes, and
/// the slot comes from the block the note was created in. Tracking each note
/// individually — rather than as an aggregate group — is what lets a withdrawal
/// client apply a per-note dust policy (by value and age) under a
/// permissionless deposit stream.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelNote {
    pub note_id: NoteId,
    /// The note's value.
    pub value: Value,
    /// The key holding the note's `PoS` participation power.
    pub pk: ZkPublicKey,
    /// The block slot the note was created in — its age reference. A note's age
    /// at any tip is that tip's slot minus this. Chain-anchored, so it is
    /// deterministic and survives sequencer restarts.
    pub slot: Slot,
}

/// The channel's note set as tracked from block data.
///
/// `finalized` holds notes created below LIB and unspent at the tracked tip
/// — immune to reorgs. `unfinalized` holds notes created above LIB on the
/// tracked branch; they can disappear on a branch change (their ids are
/// branch-stable, so a re-included op recreates the same ids).
#[derive(Debug, Clone, Default)]
pub struct ChannelWalletView {
    pub finalized: Vec<ChannelNote>,
    pub unfinalized: Vec<ChannelNote>,
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
    /// A bundled inscription+transfer pinning an observed deposit,
    /// published via `publish_pin_deposit`.
    PinDeposit(PinDepositInfo),
}

impl PendingTx {
    /// The tx hash for this entry.
    #[must_use]
    pub const fn tx_hash(&self) -> TxHash {
        match self {
            Self::Inscription(i) => i.tx_hash,
            Self::AtomicWithdraw(a) => a.tx_hash,
            Self::PinDeposit(a) => a.tx_hash,
        }
    }

    /// The inscription info for this entry.
    #[must_use]
    pub const fn inscription(&self) -> &InscriptionInfo {
        match self {
            Self::Inscription(i) => i,
            Self::AtomicWithdraw(a) => &a.inscription,
            Self::PinDeposit(a) => &a.inscription,
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
    /// L1 slot of the block this tx finalized in. Every op in one Mantle tx
    /// shares the block, hence the slot, so it is stamped per-tx.
    pub l1_slot: Slot,
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
    /// A config on the channel. `parent_msg`/`this_msg` are config-lineage
    /// ids, not message-lineage ids, and the payload is empty.
    Config(InscriptionInfo),
    /// A channel transfer op on the channel: notes re-keyed/re-denominated
    /// under channel authority.
    ChannelTransfer(ChannelTransferInfo),
}
