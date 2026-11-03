use std::time::Duration;

use lb_common_http_client::Slot;
use lb_core::{
    crypto::Hash,
    header::HeaderId,
    mantle::{
        SignedOps, Value,
        channel::ChannelState,
        gas::GasCost,
        ledger::verification_mode::StandardMode,
        notes::{InputNullifiers, OutputCommitments},
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, MsgId, config::ChannelConfigOp, deposit::Metadata, inscribe::Inscription,
            },
        },
        traits::Hashable as _,
        transactions::{MantleTxBuilder, Ops, TxHash, states::Unverified},
    },
    proofs::channel_multi_sig_proof::IndexedSignature,
};
use lb_key_management_system_service::keys::{Ed25519Key, UnverifiedEd25519PublicKey, ZkPublicKey};

use super::tx_builder::sign_prepared;

const DEFAULT_RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_PUBLISH_CHANNEL_CAPACITY: usize = 256;
/// Unmined inscriptions the node may hold at once, which bounds how many a
/// block can carry: past it, publishes queue in the sequencer until those
/// ahead land. Inscriptions are small next to the block size cap, so this
/// is set by what the wallet can fund at once, not by block space.
const DEFAULT_MAX_PENDING_PUBLISH_DEPTH: usize = 100;
/// Long enough that a merely slow tx is not rebuilt: a valid tx the node
/// holds lands within a few blocks, so one unmined this long is stuck.
const DEFAULT_STALE_REFUND_SLOTS: u64 = 30;

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
    /// The finalized config-lineage tip as of `lib`. Restored so
    /// `config_tip_at` keeps resolving the right config parent after a warm
    /// restart, once the config's block has been pruned below LIB. Defaults
    /// to [`MsgId::root`] for checkpoints written before this field existed
    /// (matching the old reset-to-root behavior).
    #[serde(default = "MsgId::root")]
    pub finalized_config: MsgId,
    /// Funding record of each pending tx this sequencer submitted.
    #[serde(default)]
    pub funding: Vec<PendingFunding>,
}

/// How a pending tx was funded: when, and with which channel ops, so a stale
/// one can be re-funded.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingFunding {
    pub tx_hash: TxHash,
    /// The LIB slot the tx was funded, or last re-funded, at; expiry counts
    /// from it.
    pub funded_at: Slot,
    /// The channel ops before funding, for a tx the sequencer built and
    /// signed itself; `None` for one it only submitted, which is shed when
    /// stale instead of rebuilt.
    pub pre_fund: Option<MantleTxBuilder>,
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
/// [`Self::sign_payload`], gathers them in any order into
/// [`IndexedSignatures`](super::IndexedSignatures), which internally sorts
/// them by channel index, and submits it alongside the (unchanged) prepared
/// value. The funded transaction and its fee-transfer proof are readable via
/// [`Self::tx`] / [`Self::transfer_proof`] so signers can inspect exactly what
/// they authorize; they carry straight back into submission unmodified.
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
    pub accredited_keys: Vec<UnverifiedEd25519PublicKey>,
    /// The channel's current `configuration_threshold` — how many of the
    /// `accredited_keys` must sign for the config to be valid. `0` for an
    /// unclaimed channel.
    pub signing_threshold: u16,
}

impl PreparedChannelConfig {
    /// The channel config this prepared tx will enact.
    ///
    /// Exposed so an accredited signer can inspect what it is authorizing — the
    /// new key set and thresholds — before signing [`Self::sign_payload`],
    /// rather than signing the opaque payload blind.
    #[must_use]
    pub fn proposed_config(&self) -> &ChannelConfigOp {
        self.tx
            .iter()
            .find_map(|op| match op {
                Op::ChannelConfig(config) => Some(config),
                _ => None,
            })
            // Invariant: the SDK builds every prepared config as a
            // `[CHANNEL_CONFIG, TRANSFER(fee)]` tx and the struct's `tx` is not
            // publicly constructible, so the config op is always present.
            .expect("a prepared channel config always carries a ChannelConfig op")
    }

    /// The full transaction the collected signatures will authorize, ops in
    /// execution order. The SDK builds `[CHANNEL_CONFIG, TRANSFER(fee)]`.
    ///
    /// A multi-sig signature is over the hash of *these ops*, and the ledger
    /// accepts that same signature as proof for every channel op in the tx. A
    /// signer must therefore inspect the whole list, not only
    /// [`Self::proposed_config`]: any other channel op bundled here (a
    /// transfer, a withdraw) would be authorized by the same signature.
    #[must_use]
    pub const fn tx(&self) -> &Ops {
        &self.tx
    }

    /// The fee transfer's proof (spending the preparer's funding wallet), if
    /// the tx was funded. Not covered by the signed hash, so it does not affect
    /// what a signature authorizes; exposed so a signer can judge whether the
    /// tx is landable.
    #[must_use]
    pub const fn transfer_proof(&self) -> Option<&OpProof> {
        self.transfer_proof.as_ref()
    }

    /// Sign this prepared config with `signing_key`.
    ///
    /// Convenience wrapper over [`sign_prepared`](super::sign_prepared) for the
    /// common case where the signer holds the prepared value. Signs the hash of
    /// [`Self::tx`] and indexes it against [`Self::accredited_keys`].
    ///
    /// The payload is derived from `tx`, never taken on trust: if
    /// [`Self::sign_payload`] does not match the hash of the ops, signing is
    /// refused, so a signer that inspected `tx` cannot be handed a payload for
    /// a different transaction. Also returns [`Error`] if `signing_key` is not
    /// among the accredited keys.
    pub fn sign_with(&self, signing_key: &Ed25519Key) -> Result<IndexedSignature, Error> {
        let tx_hash = self.tx.hash();
        let payload = tx_hash.as_signing_bytes();
        if payload.as_ref() != self.sign_payload.as_slice() {
            return Err(Error::Network(
                "sign_payload does not match the hash of tx; refusing to sign a payload the \
                 inspected ops do not account for"
                    .into(),
            ));
        }
        sign_prepared(signing_key, &self.accredited_keys, payload)
    }
}

/// A tx reported in a [`ChannelUpdate`]'s `adopted`/`orphaned`.
///
/// Variants mirror the submission flows, so an orphaned entry is recovered with
/// the method that produced it (the SDK fills a fresh `parent_msg` each time):
/// - [`ChannelUpdateTx::Inscription`] →
///   [`publish`](super::SequencerHandle::publish) with `info.payload`.
/// - [`ChannelUpdateTx::Config`] / [`ChannelUpdateTx::Custom`] →
///   caller-recovered; the SDK cannot re-sign a multi-sig config or rebuild a
///   custom tx, so it never auto-resubmits them.
#[derive(Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "the signed tx variants are larger than an inscription; boxing would change the \
              public shape of the variants"
)]
pub enum ChannelUpdateTx {
    /// A published message.
    Inscription(InscriptionInfo),
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
    /// LIB slots a pending tx may stay unmined before it is re-funded or
    /// orphaned; `0` disables.
    pub stale_refund_slots: u64,
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
            max_pending_publish_depth: DEFAULT_MAX_PENDING_PUBLISH_DEPTH,
            stale_refund_slots: DEFAULT_STALE_REFUND_SLOTS,
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
    /// The channel moved under the submission: the position it chains on
    /// already has a pending continuation, or, for a prepared bundle, the
    /// config (keys/threshold the signatures were collected under) or the
    /// message tip changed since prepare. Recoverable: re-prepare on the
    /// channel's current state and re-collect signatures.
    #[error("channel state changed: {0}")]
    ChannelStateChanged(String),
    /// The channel state is unchanged since prepare, yet the bundle's
    /// signatures would never verify on the ledger: the bundled inscription
    /// was signed by another sequencer, or the collected set is malformed
    /// (wrong count, index outside the accredited keys, or a signature that
    /// fails against its key). Not recoverable by retrying.
    #[error("bundle signatures rejected: {0}")]
    InvalidMultiSig(String),
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
#[expect(
    clippy::large_enum_variant,
    reason = "one event per block; boxing would change the public shape of `BlocksProcessed`"
)]
pub enum Event {
    /// Fires per ingested block. Carries how the channel view moved
    /// (`channel_update`), the deposits that turned canonical with it, and
    /// the finalized txs; any of them may be empty. Cold-start
    /// backfill emits one `BlocksProcessed` per batch with an empty
    /// [`ChannelUpdate::Extension`]: it walks finalized history, so there is
    /// no view change to report.
    BlocksProcessed {
        checkpoint: SequencerCheckpoint,
        channel_update: ChannelUpdate,
        /// Channel deposits observed in the blocks this event covers, in
        /// block and op order, surfaced non-finalized.
        ///
        /// Observations, not view: reconcile against branch state, a branch
        /// change can re-surface the same deposit or reorg it out.
        deposits: Vec<DepositInfo>,
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
    /// Turn-to-write status update for this sequencer.
    ///
    /// Emitted on the same change boundary as the `turn_to_write` watch
    /// channel (excluding `current_slot`-only updates), after the
    /// `BlocksProcessed` of the block that changed the turn when one did.
    TurnNotification { notification: TurnNotification },
}

/// How the channel view moved across one [`Event::BlocksProcessed`].
///
/// The channel is an ordered chain of inscriptions, with configs on their own
/// lineage beside it. Either can momentarily fork — competing entries chain
/// off the same parent — and this reports how the canonical view moved since
/// the last event. Two ways to consume it:
/// - Diff: revert every `orphaned` entry, apply every `adopted` entry. For
///   consumers that can undo an entry's effects.
/// - Rebuild from LIB: on an [`Self::Extension`] append `adopted`; on a
///   [`Self::Conflict`] recompute non-finalized state from
///   [`Self::canonical_chain`] on top of finalized state. `orphaned` is then
///   only input to republish decisions. For consumers that cannot undo.
///
/// Either way, process the orphans so no useful work is lost — e.g. if your
/// inscriptions carry Zone transactions, return them to your mempool.
/// Reprocessing is idempotent: anything still valid is already pending and
/// no-ops, so only genuinely-dead work is re-sent.
#[derive(Debug, Clone)]
pub enum ChannelUpdate {
    /// Nothing the consumer holds became invalid: per lineage, the new view
    /// is the previous view followed by `adopted`. `adopted` is empty when
    /// the view did not move at all.
    Extension {
        /// Entries that entered the view, in lineage order: every tx that
        /// advanced the canonical message or config tip. Only entries the
        /// sequencer wasn't already tracking — its own publishes apply to
        /// consumer state at publish time and don't echo back.
        adopted: Vec<ChannelUpdateTx>,
    },
    /// Entries left the view: a competing entry took their parent, a
    /// competing spend took a bundle's inputs, or a config change invalidated
    /// the pending tail. `orphaned` is never empty.
    Conflict {
        /// The non-finalized view at the new tip minus `adopted`, in lineage
        /// order: mined entries from LIB to the fork point, then the pending
        /// tail still chaining on it — the sequencer's own publishes and
        /// observed entries that dropped out of a block without being
        /// replaced. Sized by the finality depth: the whole non-finalized
        /// view is carried.
        common_prefix: Vec<ChannelUpdateTx>,
        /// Entries that entered the view, in lineage order. The full delta:
        /// entries can move between branches, so consumers dedup by
        /// `this_msg` against their own state. A publish of ours appears here
        /// only after it was reported orphaned.
        adopted: Vec<ChannelUpdateTx>,
        /// Entries that left the view: ones that were on chain, plus our own
        /// pending that can no longer land, plus stale multi-sig and custom
        /// txs. Revert from state and treat as republish candidates; see
        /// [`ChannelUpdateTx`] for how to republish each variant.
        orphaned: Vec<ChannelUpdateTx>,
    },
}

impl ChannelUpdate {
    /// Entries that entered the view.
    #[must_use]
    pub fn adopted(&self) -> &[ChannelUpdateTx] {
        match self {
            Self::Extension { adopted } | Self::Conflict { adopted, .. } => adopted,
        }
    }

    /// Entries that left the view; empty on an extension.
    #[must_use]
    pub fn orphaned(&self) -> &[ChannelUpdateTx] {
        match self {
            Self::Extension { .. } => &[],
            Self::Conflict { orphaned, .. } => orphaned,
        }
    }

    /// The whole non-finalized view at the new tip. Message entries are in
    /// lineage order, config entries are in lineage order; the two lineages
    /// are not interleaved with each other. `None` on an extension, where the
    /// view is the previous one plus `adopted`.
    #[must_use]
    pub fn canonical_chain(&self) -> Option<impl Iterator<Item = &ChannelUpdateTx>> {
        match self {
            Self::Extension { .. } => None,
            Self::Conflict {
                common_prefix,
                adopted,
                ..
            } => Some(common_prefix.iter().chain(adopted)),
        }
    }
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

/// Channel config observed in a finalized L1 block, with what it set.
#[derive(Debug, Clone)]
pub struct ConfigInfo {
    /// The transaction hash containing this config.
    pub tx_hash: TxHash,
    /// The config this one chains from, on the config lineage.
    pub parent_msg: MsgId,
    /// This config's id on the config lineage.
    pub this_msg: MsgId,
    /// The config as mined: accredited keys, thresholds, posting rules.
    pub config: ChannelConfigOp,
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
    pub signer: Option<UnverifiedEd25519PublicKey>,
}

/// A channel deposit observed in an L1 block.
///
/// Sequencers do not publish deposits; these are pure observations enriched
/// with the deposit `outputs` from the chain events API. Reported in
/// `BlocksProcessed::deposits` each time its block joins the canonical chain,
/// like an adopted inscription, and in `finalized` once final.
#[derive(Debug, Clone)]
pub struct DepositInfo {
    /// The transaction hash containing this deposit op.
    pub tx_hash: TxHash,
    /// The `op_id` of the deposit op (stable identity within the tx).
    pub op_id: Hash,
    /// Target channel.
    pub channel_id: ChannelId,
    /// Nullifiers of the ledger notes the deposit consumed.
    pub inputs: InputNullifiers,
    /// Commitments of the notes the deposit created in the channel's note
    /// set, sourced from the block's events.
    pub outputs: OutputCommitments,
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
}

impl PendingTx {
    /// The tx hash for this entry.
    #[must_use]
    pub const fn tx_hash(&self) -> TxHash {
        match self {
            Self::Inscription(i) => i.tx_hash,
        }
    }

    /// The inscription info for this entry.
    #[must_use]
    pub const fn inscription(&self) -> &InscriptionInfo {
        match self {
            Self::Inscription(i) => i,
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
    /// A config on the channel, with the keys and rules it set.
    /// `parent_msg`/`this_msg` are config-lineage ids, not message-lineage
    /// ids.
    Config(ConfigInfo),
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use lb_core::mantle::{
        Op,
        channel::{SlotTimeframe, SlotTimeout},
        notes::{NoteNullifier, OutputCommitments},
        ops::channel::{
            ChannelId, MsgId, VerifiedChannelKeys, config::ChannelConfigOp,
            withdraw::ChannelWithdrawOp,
        },
        traits::Hashable as _,
        transactions::Ops,
    };
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::{Ed25519Key, Ed25519PublicKey};

    use super::{Error, PreparedChannelConfig, sign_prepared};

    /// The signing payload the SDK derives for `ops`.
    fn payload_of(ops: &Ops) -> Vec<u8> {
        ops.hash().as_signing_bytes().to_vec()
    }

    fn config_op(keys: Vec<Ed25519PublicKey>) -> ChannelConfigOp {
        ChannelConfigOp::new(
            ChannelId::from([7; 32]),
            MsgId::root(),
            VerifiedChannelKeys::new_unchecked(keys),
            SlotTimeframe::from(15),
            SlotTimeout::from(3),
            NonZero::<u16>::MIN,
        )
        .unwrap()
    }

    #[test]
    fn proposed_config_exposes_the_built_config() {
        let new_keys: Vec<Ed25519PublicKey> = [1u8, 2, 3]
            .into_iter()
            .map(|b| Ed25519Key::from_bytes(&[b; 32]).public_key())
            .collect();
        let op = config_op(new_keys);
        let prepared = PreparedChannelConfig {
            tx: Ops::new_unchecked(vec![Op::ChannelConfig(op.clone())]),
            transfer_proof: None,
            sign_payload: vec![0xab; 32],
            accredited_keys: Vec::new(),
            signing_threshold: 0,
        };

        // Decodes the enacted config straight back out of the opaque `tx`.
        assert_eq!(prepared.proposed_config(), &op);
    }

    #[test]
    fn sign_with_delegates_to_sign_prepared() {
        let signer = Ed25519Key::from_bytes(&[5; 32]);
        let accredited = vec![
            Ed25519Key::from_bytes(&[4; 32]).public_key(),
            signer.public_key(),
        ];
        let accredited_as_unverified_keys = accredited
            .iter()
            .map(|k| k.into_unverified())
            .collect::<Vec<_>>();
        let tx = Ops::new_unchecked(vec![Op::ChannelConfig(config_op(accredited))]);
        let prepared = PreparedChannelConfig {
            sign_payload: payload_of(&tx),
            tx,
            transfer_proof: None,
            accredited_keys: accredited_as_unverified_keys.clone(),
            signing_threshold: 2,
        };

        assert_eq!(
            prepared.sign_with(&signer).expect("signer is accredited"),
            sign_prepared(
                &signer,
                &accredited_as_unverified_keys,
                &prepared.sign_payload
            )
            .expect("signer is accredited"),
        );
    }

    #[test]
    fn tx_exposes_every_op_in_order_so_a_bundled_op_is_visible() {
        let accredited = vec![Ed25519Key::from_bytes(&[4; 32]).public_key()];
        let accredited_as_unverified_keys =
            accredited.iter().map(|k| k.into_unverified()).collect();
        let config = Op::ChannelConfig(config_op(accredited));
        // A preparer smuggling a withdraw in alongside the config: one
        // signature over the tx hash would authorize both.
        let smuggled = Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id: ChannelId::from([7; 32]),
            inputs: [NoteNullifier(Fr::from(1u64))].into(),
            commitment_merkle_root: Fr::from(0u64),
            outputs: OutputCommitments::default(),
        });
        let tx = Ops::new_unchecked(vec![config.clone(), smuggled.clone()]);
        let prepared = PreparedChannelConfig {
            sign_payload: payload_of(&tx),
            tx,
            transfer_proof: None,
            accredited_keys: accredited_as_unverified_keys,
            signing_threshold: 1,
        };

        // `proposed_config` alone looks innocent…
        assert!(matches!(prepared.proposed_config(), c if Op::ChannelConfig(c.clone()) == config));
        // …but the full tx shows the extra op, in execution order.
        let ops: Vec<&Op> = prepared.tx().iter().collect();
        assert_eq!(ops, vec![&config, &smuggled]);
        assert!(prepared.transfer_proof().is_none());
    }

    #[test]
    fn sign_with_refuses_a_payload_that_does_not_match_tx() {
        let signer = Ed25519Key::from_bytes(&[5; 32]);
        let accredited = vec![signer.public_key()];
        let accredited_as_unverified_keys =
            accredited.iter().map(|k| k.into_unverified()).collect();
        let tx = Ops::new_unchecked(vec![Op::ChannelConfig(config_op(accredited))]);
        // Honest-looking ops, but the payload belongs to some other tx.
        let prepared = PreparedChannelConfig {
            tx,
            transfer_proof: None,
            sign_payload: vec![0x11; 32],
            accredited_keys: accredited_as_unverified_keys,
            signing_threshold: 1,
        };

        assert!(matches!(
            prepared.sign_with(&signer),
            Err(Error::Network(_))
        ));
    }
}
