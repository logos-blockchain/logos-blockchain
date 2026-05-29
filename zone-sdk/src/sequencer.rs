use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant, SystemTime},
};

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{ChainServiceInfo, ProcessedBlockEvent, Slot};
use lb_core::{
    crypto::Hash,
    header::HeaderId,
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _, Value,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        encoding::Ops,
        ledger::Outputs,
        ops::{
            Op, OpId as _, OpProof,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId,
                config::{ChannelConfigOp, Keys},
                inscribe::{Inscription, InscriptionOp},
                withdraw::ChannelWithdrawOp,
            },
        },
        tx::TxHash,
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::{
    adapter,
    adapter::{BoxStream, build_deposit_amounts},
    state::{
        AtomicWithdrawInfo, DepositInfo, FinalizedOp, FinalizedTx, InscriptionInfo, PublishedTx,
        TxState, WithdrawInfo,
    },
};

const TARGET: &str = "zone_sdk::sequencer";

const DEFAULT_RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_PUBLISH_CHANNEL_CAPACITY: usize = 256;
const BACKFILL_BATCH_SIZE: u64 = 100;

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
/// - [`OrphanedTx::Inscription`] → [`SequencerHandle::publish_message`] with
///   `info.payload`
/// - [`OrphanedTx::AtomicWithdraw`] →
///   [`SequencerHandle::publish_atomic_withdraw`] with
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
pub struct SlotClock {
    slot_duration: Duration,
    chain_start_time: SystemTime,
    last_observed_slot: Slot,
    last_observed_at: Instant,
}

impl SlotClock {
    #[must_use]
    pub fn from_chain_start_time(chain_start_time: SystemTime, slot_duration: Duration) -> Self {
        let current_slot = slot_from_u64(
            SystemTime::now()
                .duration_since(chain_start_time)
                .ok()
                .map_or(0, |elapsed| slots_from_duration(elapsed, slot_duration)),
        );

        Self {
            slot_duration,
            chain_start_time,
            last_observed_slot: current_slot,
            last_observed_at: Instant::now(),
        }
    }

    #[must_use]
    pub fn from_observed_slot(observed_slot: Slot, slot_duration: Duration) -> Self {
        let chain_start_time = SystemTime::now()
            .checked_sub(duration_mul(slot_duration, slot_to_u64(observed_slot)))
            .unwrap_or(SystemTime::UNIX_EPOCH);

        Self {
            slot_duration,
            chain_start_time,
            last_observed_slot: observed_slot,
            last_observed_at: Instant::now(),
        }
    }

    pub fn observe_slot(&mut self, observed_slot: Slot) {
        self.chain_start_time = SystemTime::now()
            .checked_sub(duration_mul(self.slot_duration, slot_to_u64(observed_slot)))
            .unwrap_or(self.chain_start_time);
        self.last_observed_slot = observed_slot;
        self.last_observed_at = Instant::now();
    }

    #[must_use]
    pub fn current_slot(&self) -> Slot {
        let from_chain_start = SystemTime::now()
            .duration_since(self.chain_start_time)
            .ok()
            .map_or(0, |elapsed| {
                slots_from_duration(elapsed, self.slot_duration)
            });
        let from_anchor = slot_to_u64(self.last_observed_slot).saturating_add(slots_from_duration(
            self.last_observed_at.elapsed(),
            self.slot_duration,
        ));

        slot_from_u64(from_chain_start.max(from_anchor))
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
    const fn new(channel_id: ChannelId) -> Self {
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

const fn slots_from_duration(elapsed: Duration, slot_duration: Duration) -> u64 {
    let divisor = slot_duration.as_nanos();
    if divisor == 0 {
        return 0;
    }
    let slots = elapsed.as_nanos() / divisor;
    if slots > u64::MAX as u128 {
        u64::MAX
    } else {
        slots as u64
    }
}

fn duration_mul(duration: Duration, n: u64) -> Duration {
    let nanos = duration.as_nanos().saturating_mul(u128::from(n));
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

const fn slot_to_u64(slot: Slot) -> u64 {
    slot.into_inner()
}

fn slot_from_u64(value: u64) -> Slot {
    Slot::from(value)
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
        /// via [`SequencerHandle::publish_message`]. For
        /// [`OrphanedTx::AtomicWithdraw`] entries, the consumer republishes
        /// via [`SequencerHandle::publish_atomic_withdraw`] with the original
        /// payload and reconstructed [`WithdrawArg`]s from the bundle's
        /// `withdraws`. The SDK fills fresh `parent_msg` and current
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
    /// The inner [`PublishedTx`] variant tells the consumer whether this came
    /// from [`SequencerHandle::publish_message`] or
    /// [`SequencerHandle::publish_atomic_withdraw`]. `this_msg` on the
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

enum ActorRequest {
    /// Create/sign/submit a transaction with an inscription
    PublishMessage { data: Inscription },
    /// Build an unsigned tx for the given ops and an inscription
    ///
    /// Calling this multiple times without submitting the prepared txs via
    /// `SubmitSignedTx` can cause parent msg ID conflicts, so ensure
    /// prepared txs are submitted promptly. If additional prepares are
    /// unavoidable, handle potential conflicts carefully.
    PrepareTx {
        ops: Ops,
        msg: Inscription,
        reply: tokio::sync::oneshot::Sender<Result<(MantleTx, MsgId, Ed25519Signature), Error>>,
    },
    /// Sign a tx using the sequencer's key
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw).
    SignTx {
        tx_hash: TxHash,
        reply: tokio::sync::oneshot::Sender<Result<Ed25519Signature, Error>>,
    },
    /// Submit a signed tx associated with a msg ID
    SubmitSignedTx {
        tx: SignedMantleTx,
        msg_id: MsgId,
        reply: tokio::sync::oneshot::Sender<Result<PublishResult, Error>>,
    },
    ChannelConfig {
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
        reply: tokio::sync::oneshot::Sender<Result<(SignedMantleTx, PublishResult), Error>>,
    },
    /// Publish an atomic inscription+withdraw bundle.
    ///
    /// SDK queries channel state to fill `withdraw_nonce`s and locate its own
    /// accredited-key index, builds the `MantleTx`, signs locally, and submits.
    /// Scoped to single-sequencer (centralized) channels — the sequencer's
    /// own signature is the only one used. Fire-and-forget; the result is
    /// delivered via `Event::Published`.
    PublishAtomicWithdraw {
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    },
}

enum InFlight {
    SubmittedBatch {
        results: Vec<(InscriptionId, Result<(), String>)>,
    },
}

/// Handle for submitting requests to the sequencer from other tasks.
///
/// This is cheaply cloneable and can be shared across tasks.
#[derive(Clone)]
pub struct SequencerHandle<Node> {
    request_tx: mpsc::Sender<ActorRequest>,
    node: Node,
    event_tx: broadcast::Sender<Event>,
    ready_rx: watch::Receiver<bool>,
    channel_view_rx: watch::Receiver<SequencerChannelView>,
    turn_to_write_rx: watch::Receiver<TurnNotification>,
}

impl<Node> SequencerHandle<Node>
where
    Node: adapter::Node + Sync,
{
    /// Wait until the sequencer is connected and ready to accept requests.
    pub async fn wait_ready(&mut self) {
        while !*self.ready_rx.borrow_and_update() {
            if self.ready_rx.changed().await.is_err() {
                return; // sequencer dropped
            }
        }
    }

    #[must_use]
    pub fn subscribe_channel_view(&self) -> watch::Receiver<SequencerChannelView> {
        self.channel_view_rx.clone()
    }

    #[must_use]
    pub fn subscribe_turn_to_write(&self) -> watch::Receiver<TurnNotification> {
        self.turn_to_write_rx.clone()
    }

    /// Publish an inscription to the zone's channel.
    ///
    /// Fire-and-forget: the inscription is queued for processing by the
    /// sequencer's event loop. The result (inscription ID) is delivered via
    /// [`Event::Published`] once the tx is created and posted to the network.
    ///
    /// Returns [`Error::Unavailable`] if the sequencer is not ready (cold
    /// start before the first live block, or mid-reconnect after a stream
    /// drop). Consumers driving the event loop can wait for the next
    /// [`Event::Readiness`] with `ready: true` and retry.
    pub async fn publish_message(&self, data: Inscription) -> Result<(), Error> {
        if !*self.ready_rx.borrow() {
            return Err(Error::Unavailable {
                reason: "sequencer not yet ready",
            });
        }
        self.request_tx
            .send(ActorRequest::PublishMessage { data })
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })
    }

    /// Build a [`MantleTx`] for the given ops and an inscription message,
    /// without submitting it.
    ///
    /// The returned [`MantleTx`] should be signed by all parties and submitted
    /// via [`Self::submit_signed_tx`].
    pub async fn prepare_tx(
        &self,
        ops: Ops,
        data: Inscription,
    ) -> Result<(MantleTx, MsgId, Ed25519Signature), Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::PrepareTx {
            ops,
            msg: data,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })?
    }

    /// Sign a [`MantleTx`] using the sequencer's key.
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw).
    pub async fn sign_tx(&self, tx: &MantleTx) -> Result<Ed25519Signature, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::SignTx {
            tx_hash: tx.hash(),
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        let result = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })??;

        Ok(result)
    }

    /// Submit a [`SignedMantleTx`] that is associated with a [`MsgId`]
    pub async fn submit_signed_tx(
        &self,
        tx: SignedMantleTx,
        msg_id: MsgId,
    ) -> Result<PublishResult, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::SubmitSignedTx {
            tx: tx.clone(),
            msg_id,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        let result = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })??;

        info!(target: TARGET,
            "Submitted tx including inscription {:?}",
            result.inscription_id
        );

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self.node.post_transaction(tx).await {
            warn!(target: TARGET, "Failed to post transaction: {e}");
        }

        Ok(result)
    }

    /// Update the channel's config.
    ///
    /// The sequencer's signing key must be the channel administrator
    /// (`keys[0]`). This overwrites the entire key list — include the admin
    /// key if it should remain authorized.
    ///
    /// `posting_timeframe` and `posting_timeout` control round-robin
    /// sequencer rotation (see Mantle spec). Pass `0` for both to keep a
    /// single fixed sequencer at index 0.
    ///
    /// Returns the publish result (with checkpoint) and a future that
    /// resolves when the transaction is finalized.
    pub async fn channel_config(
        &self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
    ) -> Result<(PublishResult, impl Future<Output = Result<(), Error>>), Error> {
        // Subscribe BEFORE submitting to avoid missing finalization events.
        let mut event_rx = self.event_tx.subscribe();

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::ChannelConfig {
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            withdraw_threshold,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })?;

        let (signed_tx, publish_result) = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "sequencer dropped reply",
        })??;

        let tx_hash = signed_tx.mantle_tx.hash();

        info!(target: TARGET, "Submitted channel_config transaction {}", hex::encode(tx_hash.0));

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self.node.post_transaction(signed_tx).await {
            warn!(target: TARGET, "Failed to post channel_config transaction: {e}");
        }

        let finalized = async move {
            loop {
                match event_rx.recv().await {
                    Ok(Event::TxsFinalized { ref items })
                        if items.iter().any(|i| i.tx_hash == tx_hash) =>
                    {
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(_) => {
                        return Err(Error::Unavailable {
                            reason: "sequencer stopped",
                        });
                    }
                }
            }
        };

        Ok((publish_result, finalized))
    }

    /// Publish an atomic inscription+withdraw bundle.
    ///
    /// The SDK queries channel state to fill withdraw nonces and locate its
    /// own accredited-key index, selects the inscription's `parent_msg` from
    /// the current canonical tip, builds the bundled `MantleTx`, signs locally
    /// with the sequencer's key, and submits. Scoped to single-sequencer
    /// (centralized) channels — only the sequencer's own signature is used.
    ///
    /// Fire-and-forget: the bundle is queued for processing by the sequencer's
    /// event loop. The result is delivered via
    /// [`Event::Published`] (`PublishedTx::AtomicWithdraw` variant). Safe to
    /// call from the drive task itself (e.g. an orphan re-publish handler)
    /// because it does not await an actor reply.
    ///
    /// Returns [`Error::Unavailable`] if the sequencer is not ready (cold
    /// start before the first live block, or mid-reconnect after a stream
    /// drop). Consumers driving the event loop can wait for the next
    /// [`Event::Readiness`] with `ready: true` and retry.
    pub async fn publish_atomic_withdraw(
        &self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    ) -> Result<(), Error> {
        if !*self.ready_rx.borrow() {
            return Err(Error::Unavailable {
                reason: "sequencer not yet ready",
            });
        }
        self.request_tx
            .send(ActorRequest::PublishAtomicWithdraw {
                inscribe,
                withdraws,
            })
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })
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

/// Zone sequencer.
///
/// The caller drives execution by calling [`next_event`](Self::next_event) in a
/// loop. Publish and admin operations are submitted via the [`SequencerHandle`]
/// which can be used from any task.
pub struct ZoneSequencer<Node> {
    // Config
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    node: Node,
    config: SequencerConfig,

    // Actor channel for receiving requests from other tasks
    request_rx: mpsc::Receiver<ActorRequest>,

    // State
    state: Option<TxState>,
    current_tip: Option<HeaderId>,
    lib_slot: Slot,
    last_msg_id: MsgId,
    slot_clock: Option<SlotClock>,
    channel_state: Option<ChannelState>,
    own_key_index: Option<u16>,

    // Block stream
    blocks_stream: Option<BoxStream<ProcessedBlockEvent>>,

    // Resubmission
    resubmit_interval: tokio::time::Interval,
    resubmit_active: bool,
    in_flight: FuturesUnordered<BoxFuture<'static, InFlight>>,

    // Buffered events — when multiple events occur on the same block
    buffered_events: VecDeque<Event>,

    // Incremental backfill state — processes one batch per next_event() call
    backfill_from: Option<Slot>,
    backfill_to: Option<Slot>,
    // True on cold start (no checkpoint), false otherwise. Tells
    // `setup_backfill_range` to include the genesis slot in the first
    // backfill range; cleared after that initial range is scheduled so
    // genesis is processed exactly once. A warm restart from a checkpoint
    // with `lib_slot == 0` looks identical without this flag, so we'd
    // otherwise either skip or re-process genesis depending on which side
    // of `+1` we picked.
    backfill_from_genesis: bool,

    // Broadcast channel for events — handles subscribe to receive events
    event_tx: broadcast::Sender<Event>,

    // Readiness signal — set to true when connected and backfill is complete
    ready_tx: watch::Sender<bool>,
    channel_view_tx: watch::Sender<SequencerChannelView>,
    turn_to_write_tx: watch::Sender<TurnNotification>,
}

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Create a new sequencer with default configuration.
    ///
    /// Returns the sequencer (to drive via [`next_event`](Self::next_event))
    /// and a handle (for submitting requests from other tasks).
    #[must_use]
    pub fn init(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node: Node,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> (Self, SequencerHandle<Node>) {
        Self::init_with_config(
            channel_id,
            signing_key,
            node,
            SequencerConfig::default(),
            checkpoint,
        )
    }

    /// Create a new sequencer with custom configuration.
    ///
    /// Returns immediately. The sequencer emits [`Event::Readiness`] with
    /// `ready: true` once it has connected and completed backfill.
    ///
    /// Returns the sequencer (to drive via [`next_event`](Self::next_event))
    /// and a handle (for submitting requests from other tasks).
    #[must_use]
    pub fn init_with_config(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node: Node,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> (Self, SequencerHandle<Node>) {
        let (request_tx, request_rx) = mpsc::channel(config.publish_channel_capacity);

        let (state, lib_slot, last_msg_id, backfill_from_genesis) = if let Some(cp) = checkpoint {
            info!(target: TARGET,
                "Restoring from checkpoint: {} pending txs, lib={:?}, lib_slot={:?}",
                cp.pending_txs.len(),
                cp.lib,
                cp.lib_slot
            );
            let SequencerCheckpoint {
                last_msg_id,
                pending_txs,
                lib,
                lib_slot,
            } = cp;
            let finalized_msg =
                restored_pending_channel_tip(&pending_txs, channel_id).unwrap_or(last_msg_id);
            let mut tx_state = TxState::new(lib, finalized_msg);
            for (_hash, tx) in pending_txs {
                restore_pending_tx(&mut tx_state, tx, channel_id);
            }
            (Some(tx_state), lib_slot, last_msg_id, false)
        } else {
            info!(target: TARGET, "Starting fresh (no checkpoint)");
            (None, Slot::genesis(), MsgId::root(), true)
        };

        let resubmit_interval = tokio::time::interval(config.resubmit_interval);
        let (event_tx, _) = broadcast::channel(256);
        let (ready_tx, ready_rx) = watch::channel(false);
        let (channel_view_tx, channel_view_rx) =
            watch::channel(SequencerChannelView::new(channel_id));
        let (turn_to_write_tx, turn_to_write_rx) = watch::channel(TurnNotification {
            our_turn_to_write: false,
            starting_slot: None,
            ends_at_slot: None,
            turn_to_write_slots: None,
            current_slot: None,
        });

        let handle = SequencerHandle {
            request_tx,
            node: node.clone(),
            event_tx: event_tx.clone(),
            ready_rx,
            channel_view_rx,
            turn_to_write_rx,
        };

        let sequencer = Self {
            channel_id,
            signing_key,
            node,
            config,
            request_rx,
            state,
            current_tip: None,
            lib_slot,
            last_msg_id,
            slot_clock: None,
            channel_state: None,
            own_key_index: None,
            blocks_stream: None,
            resubmit_interval,
            resubmit_active: false,
            in_flight: FuturesUnordered::new(),
            buffered_events: VecDeque::new(),
            backfill_from: None,
            backfill_to: None,
            backfill_from_genesis,
            event_tx,
            ready_tx,
            channel_view_tx,
            turn_to_write_tx,
        };

        (sequencer, handle)
    }

    /// Whether the sequencer is connected and ready to accept requests.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        *self.ready_tx.borrow()
    }

    /// Get the current checkpoint for persistence.
    ///
    /// Returns `None` if the sequencer has not yet initialized.
    #[must_use]
    pub fn checkpoint(&self) -> Option<SequencerCheckpoint> {
        self.state
            .as_ref()
            .map(|s| build_checkpoint(s, self.last_msg_id, self.lib_slot))
    }

    /// Drive the sequencer and return the next event.
    ///
    /// Any event returned from this method is also broadcast on `event_tx`.
    /// Events that cannot be returned immediately are queued in
    /// `buffered_events` and broadcast when later returned.
    pub async fn next_event(&mut self) -> Option<Event> {
        // Return buffered event from previous call if any.
        if let Some(event) = self.buffered_events.pop_front() {
            return Some(self.emit_now(event));
        }

        // Process incremental backfill — one batch per call.
        // Returns Some(Some(event)) or Some(None) while active, None when done.
        if let Some(maybe_event) = self.process_incremental_backfill().await {
            return maybe_event.map(|event| self.emit_now(event));
        }

        // Ensure we have a blocks stream (connects if needed).
        if !self.ensure_connected().await {
            return None;
        }

        let stream = self.blocks_stream.as_mut()?;

        tokio::select! {
            // Biased: drain queued publish/sign requests before processing new
            // block events. Prevents a race where a `ChannelUpdate`-triggered
            // republish gets re-orphaned by a fresh block event arriving on
            // the stream before the republish reaches the actor, which could
            // cause duplicate work or duplicate inscriptions.
            biased;
            Some(request) = self.request_rx.recv() => {
                self.handle_request(request)
                    .await
                    .map(|event| self.emit_now(event))
            }
            Some(inflight_result) = self.in_flight.next(), if !self.in_flight.is_empty() => {
                let mut events = self.handle_inflight(inflight_result);
                let first = events.pop_front();
                self.buffered_events.extend(events);
                first.map(|event| self.emit_now(event))
            }
            maybe_event = stream.next() => {
                self.handle_stream_item(maybe_event)
                    .await
                    .map(|event| self.emit_now(event))
            }
            _ = self.resubmit_interval.tick(), if self.current_tip.is_some() && !self.resubmit_active => {
                self.enqueue_pending_submit();
                None
            }
        }
    }

    /// Handle a single item from the blocks stream. `None` means the stream
    /// disconnected; any other value is processed as a block event.
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn handle_stream_item(
        &mut self,
        maybe_event: Option<ProcessedBlockEvent>,
    ) -> Option<Event> {
        let Some(block_event) = maybe_event else {
            warn!(target: TARGET, "Blocks stream disconnected, will reconnect on next call");
            self.blocks_stream = None;
            return self.signal_not_ready();
        };

        let result = match handle_block_event(
            &block_event,
            &mut self.state,
            &mut self.current_tip,
            &mut self.lib_slot,
            self.channel_id,
            &self.node,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("Block event processing failed; dropping stream so reconnect retries: {e}");
                self.blocks_stream = None;
                return self.signal_not_ready();
            }
        };

        if let Some(slot_clock) = self.slot_clock.as_mut() {
            slot_clock.observe_slot(block_event.tip_slot);
        }

        if let Err(err) = self.refresh_channel_state().await {
            error!(
                target: TARGET,
                "Failed to refresh channel state after block; dropping stream so reconnect retries: {err}"
            );
            self.blocks_stream = None;
            return self.signal_not_ready();
        }

        let became_ready = self.maybe_signal_ready();
        let mut events = self.apply_block_result(result);

        self.enqueue_pending_submit();

        if let Some(state) = self.state.as_ref() {
            events.push_back(Event::Checkpoint {
                checkpoint: build_checkpoint(state, self.last_msg_id, self.lib_slot),
            });
        }

        if became_ready {
            // Preserve the existing public event contract: when readiness transitions,
            // Ready is emitted first. Any block-derived events and any Published event
            // produced by queue draining are buffered and emitted on subsequent
            // next_event() calls.
            self.buffered_events.extend(events);
            return Some(self.emit_now(Event::Readiness { ready: true }));
        }

        let event = events.pop_front()?;
        self.buffered_events.extend(events);

        Some(event)
    }

    fn emit_now(&self, event: Event) -> Event {
        drop(self.event_tx.send(event.clone()));
        event
    }

    /// If not yet ready and startup backfill is complete, mark ready. Returns
    /// true if readiness transitioned.
    fn maybe_signal_ready(&self) -> bool {
        if self.is_ready() {
            return false;
        }

        if self.backfill_from.is_none() && self.backfill_to.is_none() {
            debug!(target: TARGET, "Sequencer ready (backfill complete, first block processed)");
            let _ = self.ready_tx.send(true);
            true
        } else {
            debug!(target: TARGET,
                "Not yet ready: backfill_from={:?}, backfill_to={:?}",
                self.backfill_from, self.backfill_to
            );
            false
        }
    }

    /// Flip readiness to `false` and, if that was an actual transition (was
    /// previously `true`), surface [`Event::Readiness`] so the consumer's
    /// drive loop learns about the disconnect on the event stream. Returns
    /// `None` when readiness was already `false` (no spurious event).
    fn signal_not_ready(&self) -> Option<Event> {
        let mut transitioned = false;
        self.ready_tx.send_if_modified(|current| {
            if *current {
                *current = false;
                transitioned = true;
                true
            } else {
                false
            }
        });
        self.publish_turn_to_write(false);
        transitioned.then(|| self.emit_now(Event::Readiness { ready: false }))
    }

    async fn refresh_channel_state(&mut self) -> Result<(), Error> {
        let channel = self
            .node
            .channel_state(self.channel_id)
            .await
            .map_err(|err| Error::Network(err.to_string()))?;
        self.own_key_index = channel
            .as_ref()
            .and_then(|channel| self.own_key_index_for(channel));
        self.channel_state = channel;
        Ok(())
    }

    fn channel_view(&self) -> SequencerChannelView {
        let current_slot = self
            .slot_clock
            .as_ref()
            .map_or(Slot::genesis(), SlotClock::current_slot);

        let authorized_key_index = self
            .channel_state
            .as_ref()
            .map(|channel| channel.round_robin(current_slot).0);

        let tip_message = self
            .channel_state
            .as_ref()
            .map_or(self.last_msg_id, |channel| channel.tip_message);

        let posting_timeframe = self
            .channel_state
            .as_ref()
            .map(|channel| u32::from(channel.posting_timeframe.clone()));

        let posting_timeout = self
            .channel_state
            .as_ref()
            .map(|channel| u32::from(channel.posting_timeout.clone()));

        let accredited_key_count = self
            .channel_state
            .as_ref()
            .map(|channel| channel.accredited_keys.len());

        let pending_publish_txs = self
            .state
            .as_ref()
            .map_or(0, TxState::pending_publish_count);

        SequencerChannelView {
            channel_id: self.channel_id,
            channel: self.channel_state.clone(),
            current_slot,
            own_key_index: self.own_key_index,
            authorized_key_index,
            our_turn_to_write: self.can_publish_inscription_now(),
            tip_message,
            pending_publish_txs,
            queued_messages: pending_publish_txs,
            turn_to_write_slots: posting_timeframe,
            posting_timeout_slots: posting_timeout,
            accredited_key_count,
        }
    }

    fn publish_channel_view(&self) {
        let view = self.channel_view();
        let turn_to_write = self.is_ready() && view.our_turn_to_write;
        drop(self.channel_view_tx.send(view));
        self.publish_turn_to_write(turn_to_write);
    }

    fn publish_turn_to_write(&self, turn_to_write: bool) {
        let mut emitted: Option<TurnNotification> = None;

        self.turn_to_write_tx.send_if_modified(|current| {
            let new = self.turn_notification(turn_to_write);
            let changed = current.our_turn_to_write != new.our_turn_to_write
                || current.starting_slot != new.starting_slot
                || current.ends_at_slot != new.ends_at_slot
                || current.turn_to_write_slots != new.turn_to_write_slots;

            *current = new.clone();
            if changed {
                emitted = Some(new);
            }

            changed
        });

        if let Some(notification) = emitted {
            drop(self.event_tx.send(Event::TurnNotification { notification }));
        }
    }

    fn turn_notification(&self, our_turn_to_write: bool) -> TurnNotification {
        let Some(slot_clock) = &self.slot_clock else {
            return TurnNotification {
                our_turn_to_write,
                starting_slot: None,
                ends_at_slot: None,
                turn_to_write_slots: None,
                current_slot: None,
            };
        };

        let current_slot = slot_clock.current_slot();
        let Some(channel) = &self.channel_state else {
            return TurnNotification {
                our_turn_to_write,
                starting_slot: None,
                ends_at_slot: None,
                turn_to_write_slots: None,
                current_slot: Some(slot_to_u64(current_slot)),
            };
        };

        let (_, turn_start_slot) = channel.round_robin(current_slot);
        let turn_to_write_slots = u32::from(channel.posting_timeframe.clone());
        let starting_slot = slot_to_u64(turn_start_slot);
        let ends_at_slot = starting_slot.saturating_add(u64::from(turn_to_write_slots));

        TurnNotification {
            our_turn_to_write,
            starting_slot: Some(starting_slot),
            ends_at_slot: Some(ends_at_slot),
            turn_to_write_slots: Some(turn_to_write_slots),
            current_slot: Some(slot_to_u64(current_slot)),
        }
    }

    fn own_key_index_for(&self, channel: &ChannelState) -> Option<u16> {
        channel
            .accredited_keys
            .iter()
            .position(|pk| *pk == self.signing_key.public_key())
            .map(|idx| idx as u16)
    }

    fn can_publish_inscription_now(&self) -> bool {
        let Some(slot_clock) = &self.slot_clock else {
            return false;
        };
        let current_slot = slot_clock.current_slot();

        let Some(channel) = &self.channel_state else {
            // A missing channel is the normal pre-genesis-inscription state.
            // Network/query failures are surfaced before this point, so this
            // still only publishes when the absence is known.
            return true;
        };

        let Some(own_idx) = self.own_key_index else {
            return false;
        };

        let (authorized_idx, turn_start_slot) = channel.round_robin(current_slot);
        authorized_idx == own_idx
            && self.has_enough_turn_time_left(channel, current_slot, turn_start_slot)
    }

    fn has_enough_turn_time_left(
        &self,
        channel: &ChannelState,
        current_slot: Slot,
        turn_start_slot: Slot,
    ) -> bool {
        let min_remaining = self.config.min_slots_remaining_in_turn;
        let posting_timeframe = u32::from(channel.posting_timeframe.clone());
        if min_remaining == 0 || posting_timeframe == 0 {
            return true;
        }

        let turn_end_slot =
            slot_to_u64(turn_start_slot).saturating_add(u64::from(posting_timeframe));
        turn_end_slot.saturating_sub(slot_to_u64(current_slot)) >= min_remaining
    }

    /// Enqueue pending signed txs for posting. Channel inscriptions are posted
    /// only while the round-robin gate is open. First-time publish posts are
    /// bounded by `max_pending_publish_depth`; already-posted pending txs
    /// may still be reposted for mempool recovery.
    fn enqueue_pending_submit(&mut self) {
        if self.resubmit_active || !self.in_flight.is_empty() {
            return;
        }

        let Some(tip) = self.current_tip else {
            self.publish_channel_view();
            return;
        };

        let Some(state) = self.state.as_ref() else {
            self.publish_channel_view();
            return;
        };

        let can_publish_inscription = self.can_publish_inscription_now();
        let pending = state.pending_txs(tip);
        let mut submit = Vec::new();
        let mut active_publish_count = state.posted_pending_publish_count();
        let max_depth = self.config.max_pending_publish_depth.max(1);

        for (id, signed_tx) in pending {
            let pending_inscription_publish = state.pending_inscription(&id);
            let is_inscription_publish = pending_inscription_publish.is_some();
            if is_inscription_publish && !can_publish_inscription {
                continue;
            }

            let is_first_inscription_post =
                pending_inscription_publish.is_some_and(|pending| !pending.posted);
            if is_inscription_publish
                && is_first_inscription_post
                && active_publish_count >= max_depth
            {
                break;
            }

            if is_inscription_publish && is_first_inscription_post {
                active_publish_count = active_publish_count.saturating_add(1);
            }
            submit.push((id, signed_tx));
        }

        if submit.is_empty() {
            self.publish_channel_view();
            return;
        }

        debug!(target: TARGET, "Submitting {} pending transaction(s)", submit.len());
        let node = self.node.clone();
        self.resubmit_active = true;
        self.in_flight.push(Box::pin(async move {
            let mut results = Vec::with_capacity(submit.len());
            for (id, tx) in submit {
                let result = node
                    .post_transaction(tx)
                    .await
                    .map_err(|err| err.to_string());
                results.push((id, result));
            }
            InFlight::SubmittedBatch { results }
        }));
        self.publish_channel_view();
    }

    fn handle_inflight(&mut self, event: InFlight) -> VecDeque<Event> {
        self.resubmit_active = false;
        let mut events = VecDeque::new();

        match event {
            InFlight::SubmittedBatch { results } => {
                for (id, result) in results {
                    if let Err(err) = result {
                        warn!(target: TARGET, "Failed to submit pending transaction {}: {err}", hex::encode(id.0));
                        continue;
                    }

                    let first_post = self
                        .state
                        .as_mut()
                        .is_some_and(|state| state.mark_pending_inscription_posted(&id));
                    if first_post && let Some(event) = self.published_event(id) {
                        events.push_back(event);
                    }
                }
            }
        }

        self.publish_channel_view();
        events
    }

    fn published_event(&self, id: InscriptionId) -> Option<Event> {
        let state = self.state.as_ref()?;
        let pending = state.pending_inscription(&id)?;
        let info = InscriptionInfo {
            tx_hash: pending.tx_hash,
            parent_msg: pending.parent_msg,
            this_msg: pending.this_msg,
            payload: pending.payload.clone(),
        };
        let tx = match pending.withdraws.clone() {
            Some(withdraws) => PublishedTx::AtomicWithdraw(AtomicWithdrawInfo {
                tx_hash: pending.tx_hash,
                inscription: info,
                withdraws,
            }),
            None => PublishedTx::Inscription(info),
        };

        Some(Event::Published { tx: Box::new(tx) })
    }

    /// Process one batch of incremental backfill if active.
    ///
    /// Returns `Some(event)` while backfill is active (caller should return
    /// the inner value), or `None` when backfill is complete/inactive.
    async fn process_incremental_backfill(&mut self) -> Option<Option<Event>> {
        let (Some(from), Some(to)) = (self.backfill_from, self.backfill_to) else {
            return None;
        };

        let from_u64: u64 = from.into();
        let to_u64: u64 = to.into();

        if from_u64 > to_u64 {
            // Backfill exhausted — range advanced past `to` in a previous batch.
            self.backfill_from = None;
            self.backfill_to = None;
            return None;
        }

        let batch_end = (from_u64 + BACKFILL_BATCH_SIZE).min(to_u64);
        let batch = match fetch_and_process_blocks(
            self.state.as_mut().unwrap(),
            from_u64,
            batch_end,
            self.channel_id,
            &self.node,
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                error!(
                    from = from_u64,
                    to = batch_end,
                    "Backfill batch failed; will retry same range after delay: {e}"
                );
                tokio::time::sleep(self.config.reconnect_delay).await;
                // Leave `backfill_from` untouched so the next tick retries
                // the same range. Active but no event this turn.
                return Some(None);
            }
        };

        self.backfill_from = Some(Slot::from(batch_end + 1));

        // Advance the channel-tip marker using the last inscription in the
        // batch. Deposits / withdraws don't have a `this_msg` lineage.
        if let Some(last_inscription) = batch
            .items
            .iter()
            .rev()
            .flat_map(|t| t.ops.iter().rev())
            .find_map(|op| match op {
                FinalizedOp::Inscription(i) => Some(i),
                FinalizedOp::Deposit(_) | FinalizedOp::Withdraw(_) => None,
            })
        {
            self.last_msg_id = last_inscription.this_msg;
            if let Some(s) = self.state.as_mut() {
                s.set_finalized_msg(last_inscription.this_msg);
            }
        }

        // Clean up our pending set for txs that finalized in this batch.
        // Mirrors the cleanup in `handle_block_event`. Without this, restored
        // pending txs whose blocks were already finalized during downtime
        // would leak in `state.pending` and risk being mis-classified as
        // orphaned by `shed_off_branch_pending` once a live block arrives.
        if let Some(s) = self.state.as_mut() {
            for tx_hash in &batch.our_tx_hashes {
                s.remove_pending(tx_hash);
            }
        }

        self.lib_slot = Slot::from(batch_end);

        let checkpoint_event = self.state.as_ref().map(|s| Event::Checkpoint {
            checkpoint: build_checkpoint(s, self.last_msg_id, self.lib_slot),
        });

        if batch.items.is_empty() {
            return checkpoint_event.map(Some);
        }

        let event = Event::TxsFinalized { items: batch.items };
        drop(self.event_tx.send(event.clone()));
        if let Some(cp) = checkpoint_event {
            self.buffered_events.push_back(cp);
        }
        Some(Some(event))
    }

    /// Ensure the blocks stream is connected. Returns `false` if not yet
    /// ready (caller should return `None`).
    async fn ensure_connected(&mut self) -> bool {
        if self.blocks_stream.is_some() {
            return true;
        }
        debug!(target: TARGET, "ensure_connected: connecting...");

        if !self.init_state_if_needed().await {
            return false;
        }
        if !self.open_block_stream().await {
            return false;
        }
        if !self.setup_backfill_range().await {
            return false;
        }
        true
    }

    /// Initialize startup-derived sequencer state from consensus info.
    /// Preserves restored `TxState` when resuming from checkpoint, but ensures
    /// the slot clock and initial channel view are available before the
    /// sequencer is considered connected.
    ///
    /// `current_tip` stays None so the first live block event emits everything
    /// from LIB up to the new tip as `adopted`. On reconnect this is a no-op
    /// once both `state` and `slot_clock` are initialized.
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn init_state_if_needed(&mut self) -> bool {
        if self.state.is_some() && self.slot_clock.is_some() {
            return true;
        }
        match self.node.consensus_info().await {
            Ok(ChainServiceInfo {
                cryptarchia_info, ..
            }) => {
                info!(target: TARGET,
                    "Sequencer connected: tip={:?}, lib={:?}",
                    cryptarchia_info.tip, cryptarchia_info.lib
                );
                if let Err(err) = self.refresh_channel_state().await {
                    warn!(target: TARGET, "Failed to fetch initial channel state: {err}");
                    tokio::time::sleep(self.config.reconnect_delay).await;
                    return false;
                }
                if self.state.is_none() {
                    self.state = Some(TxState::new(cryptarchia_info.lib, MsgId::root()));
                }
                self.slot_clock = Some(self.build_initial_slot_clock(cryptarchia_info.slot));
                self.publish_channel_view();
                true
            }
            Err(e) => {
                warn!(target: TARGET, "Failed to fetch consensus info: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                false
            }
        }
    }

    fn build_initial_slot_clock(&self, observed_slot: Slot) -> SlotClock {
        self.config.chain_start_time.map_or_else(
            || SlotClock::from_observed_slot(observed_slot, self.config.slot_duration),
            |chain_start_time| {
                let mut slot_clock =
                    SlotClock::from_chain_start_time(chain_start_time, self.config.slot_duration);
                slot_clock.observe_slot(observed_slot);
                slot_clock
            },
        )
    }

    async fn open_block_stream(&mut self) -> bool {
        debug!(target: TARGET, "ensure_connected: opening blocks stream...");
        match self.node.block_stream().await {
            Ok(stream) => {
                debug!(target: TARGET, "ensure_connected: blocks stream connected");
                self.blocks_stream = Some(stream);
                true
            }
            Err(e) => {
                warn!(target: TARGET, "Failed to connect to blocks stream: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                false
            }
        }
    }

    /// Check whether an incremental backfill range is needed (checkpoint lib
    /// behind current network lib). Returns `false` if a backfill was set up
    /// (caller defers readiness until backfill completes).
    ///
    /// `backfill_from_genesis` selects the inclusive start: on cold start
    /// the range begins at slot 0 so genesis-inscribed channels are picked
    /// up; on a warm restart from a checkpoint, the checkpoint slot is
    /// already processed and the range starts at `from + 1`.
    async fn setup_backfill_range(&mut self) -> bool {
        if self.state.is_none() || self.backfill_from.is_some() {
            return true;
        }
        match self.node.consensus_info().await {
            Ok(ChainServiceInfo {
                cryptarchia_info, ..
            }) => {
                let network_lib_slot = cryptarchia_info.lib_slot;
                let from: u64 = self.lib_slot.into();
                let to: u64 = network_lib_slot.into();
                let (start, run) = if self.backfill_from_genesis {
                    self.backfill_from_genesis = false;
                    (from, true)
                } else {
                    (from + 1, from < to)
                };
                if run {
                    debug!(target: TARGET, "Starting incremental backfill from slot {start} to {to}");
                    self.backfill_from = Some(Slot::from(start));
                    self.backfill_to = Some(network_lib_slot);
                    return false;
                }
                true
            }
            Err(e) => {
                warn!(target: TARGET, "Failed to fetch consensus info for backfill check: {e}");
                true
            }
        }
    }

    /// Process a `BlockEventResult`: apply channel updates to local state and
    /// return the resulting block-derived events in emission order.
    ///
    /// This does not broadcast or buffer events. The caller owns event-delivery
    /// policy because block processing may be combined with readiness
    /// transitions and queued publish draining.
    fn apply_block_result(&mut self, result: BlockEventResult) -> VecDeque<Event> {
        if let Some(update) = result.channel_update.as_ref() {
            Self::log_channel_update(update);

            let has_pending = self
                .state
                .as_ref()
                .is_some_and(TxState::has_pending_inscriptions);

            if !update.orphaned.is_empty() || !has_pending {
                self.last_msg_id = update.new_channel_tip;
            }
        }

        let mut events = VecDeque::new();

        if let Some(update) = result.channel_update {
            events.push_back(self.build_channel_event(update));
        }

        if !result.finalized_items.is_empty() {
            events.push_back(Event::TxsFinalized {
                items: result.finalized_items,
            });
        }

        events
    }

    fn log_channel_update(update: &crate::state::ChannelUpdateInfo) {
        debug!(target: TARGET,
            "ChannelUpdate: orphaned={}, adopted={}, new_tip={}",
            update.orphaned.len(),
            update.adopted.len(),
            hex::encode(update.new_channel_tip.as_ref()),
        );
        for info in &update.orphaned {
            debug!(target: TARGET,
                "  orphaned: payload={:?}, tx={}, msg_id={}",
                String::from_utf8_lossy(&info.payload),
                hex::encode(info.tx_hash.0),
                hex::encode(info.this_msg.as_ref()),
            );
        }
        for info in &update.adopted {
            debug!(target: TARGET,
                "  adopted: payload={:?}, tx={}, msg_id={}",
                String::from_utf8_lossy(&info.payload),
                hex::encode(info.tx_hash.0),
                hex::encode(info.this_msg.as_ref()),
            );
        }
    }

    /// Build the `ChannelUpdate` event. `orphaned` contains only our own
    /// pending whose original signed tx is permanently invalid — items the
    /// SDK has given up on (parent slot claimed by a competing inscription,
    /// or parent transitively off canonical). Block-delta orphans whose
    /// original tx is still valid (the SDK keeps retrying them) are not
    /// surfaced. `adopted` is filtered against our internal outbox (by
    /// `this_msg`) to exclude inscriptions this instance submitted —
    /// consumers learn about those via `Event::Published`. This outbox match
    /// works under shared-signing-key deployments: each sequencer instance
    /// only tracks what it itself submitted.
    fn build_channel_event(&mut self, u: crate::state::ChannelUpdateInfo) -> Event {
        let orphaned = match (self.state.as_mut(), self.current_tip) {
            (Some(s), Some(tip)) => s.shed_off_branch_pending(tip),
            _ => Vec::new(),
        };
        let adopted: Vec<InscriptionInfo> = match self.state.as_ref() {
            Some(s) => u
                .adopted
                .into_iter()
                .filter(|i| !s.outbox_contains(i.this_msg))
                .collect(),
            None => u.adopted,
        };

        let typed_orphaned = orphaned.into_iter().filter_map(orphan_from_shed).collect();

        Event::ChannelUpdate {
            orphaned: typed_orphaned,
            adopted,
        }
    }

    async fn handle_request(&mut self, request: ActorRequest) -> Option<Event> {
        if !self.is_ready() {
            reject_not_ready(request);
            return None;
        }

        match request {
            ActorRequest::PublishMessage { data } => self.handle_publish(data),
            ActorRequest::PrepareTx { ops, msg, reply } => {
                let result = prepare_tx(
                    ops,
                    self.channel_id,
                    &self.signing_key,
                    msg,
                    self.last_msg_id,
                );
                // do not update last_msg_id since tx is not submitted yet
                drop(reply.send(Ok(result)));
                None
            }
            ActorRequest::SignTx { tx_hash, reply } => {
                let signature = sign_tx(tx_hash, &self.signing_key);
                drop(reply.send(Ok(signature)));
                None
            }
            ActorRequest::SubmitSignedTx { tx, msg_id, reply } => {
                // Safe to unwrap — is_ready() guarantees state is initialized
                let s = self.state.as_mut().unwrap();
                let result = submit_signed_tx(s, tx, msg_id, &mut self.last_msg_id);
                drop(reply.send(Ok(result)));
                None
            }
            ActorRequest::ChannelConfig {
                keys,
                posting_timeframe,
                posting_timeout,
                configuration_threshold,
                withdraw_threshold,
                reply,
            } => {
                // Safe to unwrap — is_ready() guarantees state is initialized
                let s = self.state.as_mut().unwrap();
                let signed_tx = create_channel_config_tx(
                    self.channel_id,
                    &[&self.signing_key],
                    keys,
                    posting_timeframe,
                    posting_timeout,
                    configuration_threshold,
                    withdraw_threshold,
                );
                s.submit_other(signed_tx.clone());
                let result = PublishResult {
                    inscription_id: signed_tx.mantle_tx.hash(),
                };
                drop(reply.send(Ok((signed_tx, result))));
                self.publish_channel_view();
                None
            }
            ActorRequest::PublishAtomicWithdraw {
                inscribe,
                withdraws,
            } => match self
                .handle_publish_atomic_withdraw(inscribe, withdraws)
                .await
            {
                Ok(event) => event,
                Err(e) => {
                    warn!("publish_atomic_withdraw failed: {e}");
                    None
                }
            },
        }
    }

    fn handle_publish(&mut self, data: Inscription) -> Option<Event> {
        self.build_pending_publish(data);
        self.enqueue_pending_submit();
        None
    }

    fn build_pending_publish(&mut self, data: Inscription) -> InscriptionId {
        let parent = {
            let state = self.state.as_mut().unwrap();
            if let Some(tip) = self.current_tip {
                state.publish_parent(tip)
            } else {
                self.last_msg_id
            }
        };
        let (signed_tx, new_msg_id) =
            create_inscribe_tx(self.channel_id, &self.signing_key, data.clone(), parent);
        let id = signed_tx.mantle_tx.hash();

        debug!(target: TARGET,
            "Prepared publish: payload={:?}, parent={}, msg_id={}, tx={}",
            String::from_utf8_lossy(&data),
            hex::encode(parent.as_ref()),
            hex::encode(new_msg_id.as_ref()),
            hex::encode(id.0),
        );

        let state = self.state.as_mut().unwrap();
        state.submit_inscription(signed_tx, parent, new_msg_id, data);
        self.last_msg_id = new_msg_id;

        id
    }

    /// Build, sign, and submit an atomic inscription+withdraw bundle.
    ///
    /// Scoped to centralized single-sequencer channels — the sequencer's own
    /// signature is the only signature used. Errors early if the channel's
    /// `withdraw_threshold > 1`, which would require multi-sig orchestration
    /// not supported by this API.
    async fn handle_publish_atomic_withdraw(
        &mut self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    ) -> Result<Option<Event>, Error> {
        if withdraws.is_empty() {
            return Err(Error::Network(
                "publish_atomic_withdraw requires at least one withdraw".into(),
            ));
        }

        // Query channel state for the current on-chain `withdraw_nonce` and
        // this sequencer's accredited-key index. Done before borrowing
        // `self.state` since `await` on a node method must not hold a `&Self`
        // reference (forces `Self: Sync`).
        let channel_state = self
            .node
            .channel_state(self.channel_id)
            .await
            .map_err(|e| Error::Network(format!("channel_state query failed: {e}")))?
            .ok_or_else(|| {
                Error::Network(format!(
                    "publish_atomic_withdraw requires channel state for {:?}",
                    self.channel_id
                ))
            })?;
        if channel_state.withdraw_threshold > 1 {
            return Err(Error::Network(format!(
                "publish_atomic_withdraw requires withdraw_threshold == 1, got {}",
                channel_state.withdraw_threshold
            )));
        }
        let own_key_index = find_own_key_index(&channel_state, &self.signing_key)?;
        let mut next_nonce = channel_state.withdrawal_nonce;

        // Safe to unwrap — is_ready() guarantees state is initialized
        let s = self.state.as_ref().unwrap();
        let parent = if let Some(tip) = self.current_tip {
            s.publish_parent(tip)
        } else {
            self.last_msg_id
        };

        let mut ops: Vec<Op> = Vec::with_capacity(withdraws.len() + 1);
        let mut withdraw_ops = Vec::with_capacity(withdraws.len());
        for arg in withdraws {
            let op = ChannelWithdrawOp {
                channel_id: self.channel_id,
                outputs: arg.outputs,
                withdraw_nonce: next_nonce,
            };
            withdraw_ops.push(op.clone());
            ops.push(Op::ChannelWithdraw(op));
            next_nonce = next_nonce
                .checked_add(1)
                .ok_or_else(|| Error::Network("withdraw nonce overflow".into()))?;
        }

        let inscription_op = InscriptionOp {
            channel_id: self.channel_id,
            inscription: inscribe.clone(),
            parent,
            signer: self.signing_key.public_key(),
        };
        let msg_id = inscription_op.id();
        ops.push(Op::ChannelInscribe(inscription_op));

        let tx = MantleTx(Ops::try_from(ops).map_err(|e| {
            Error::Network(format!("atomic withdraw bundle exceeds op limit: {e:?}"))
        })?);
        let own_sig = sign_tx(tx.hash(), &self.signing_key);
        let ops_proofs = build_atomic_withdraw_ops_proofs(&tx, own_key_index, own_sig)?;
        let signed_tx = SignedMantleTx::new(tx, ops_proofs)
            .map_err(|e| Error::Network(format!("signed tx assembly failed: {e:?}")))?;

        // Safe to unwrap — is_ready() guarantees state is initialized
        let s = self.state.as_mut().unwrap();

        let tx_hash = signed_tx.mantle_tx.hash();
        let withdraw_infos: Vec<WithdrawInfo> = withdraw_ops
            .into_iter()
            .map(|op| WithdrawInfo { tx_hash, op })
            .collect();
        s.submit_atomic_withdraw(signed_tx, parent, msg_id, inscribe, withdraw_infos);
        self.last_msg_id = msg_id;

        self.enqueue_pending_submit();
        Ok(None)
    }
}

/// Build per-op proofs for an atomic withdraw bundle. The same single-signer
/// `ChannelMultiSigProof` is reused for every `ChannelWithdraw` op (all sign
/// the same tx hash with the same key) and the inscription op carries an
/// `Ed25519Sig` proof.
fn build_atomic_withdraw_ops_proofs(
    tx: &MantleTx,
    own_key_index: ChannelKeyIndex,
    own_sig: Ed25519Signature,
) -> Result<Vec<OpProof>, Error> {
    let withdraw_proof =
        ChannelMultiSigProof::new(vec![IndexedSignature::new(own_key_index, own_sig)])
            .map_err(|e| Error::Network(format!("multi-sig proof assembly failed: {e:?}")))?;
    let mut ops_proofs = Vec::with_capacity(tx.ops().len());
    for op in tx.ops() {
        match op {
            Op::ChannelWithdraw(_) => {
                ops_proofs.push(OpProof::ChannelMultiSigProof(withdraw_proof.clone()));
            }
            Op::ChannelInscribe(_) => ops_proofs.push(OpProof::Ed25519Sig(own_sig)),
            _ => {
                return Err(Error::Network(format!(
                    "unexpected op in atomic withdraw bundle: {op:?}"
                )));
            }
        }
    }
    Ok(ops_proofs)
}

/// Find the position of the SDK's public key in the channel's `accredited_keys`
/// list. Returns an error if our key is not on the accredited list (we can't
/// sign for this channel).
fn find_own_key_index(
    channel_state: &ChannelState,
    signing_key: &Ed25519Key,
) -> Result<ChannelKeyIndex, Error> {
    let own_pk = signing_key.public_key();
    channel_state
        .accredited_keys
        .iter()
        .position(|k| *k == own_pk)
        .map(|i| i as ChannelKeyIndex)
        .ok_or_else(|| Error::Network("sequencer key not in channel accredited_keys".into()))
}

fn reject_not_ready(request: ActorRequest) {
    let err = || Error::Unavailable {
        reason: "sequencer not yet ready",
    };
    match request {
        ActorRequest::PublishMessage { .. } | ActorRequest::PublishAtomicWithdraw { .. } => {
            warn!("Publish dropped: sequencer not yet ready");
        }
        ActorRequest::ChannelConfig { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::PrepareTx { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::SignTx { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::SubmitSignedTx { reply, .. } => drop(reply.send(Err(err()))),
    }
}

fn submit_signed_tx(
    state: &mut TxState,
    tx: SignedMantleTx,
    msg_id: MsgId,
    last_msg_id: &mut MsgId,
) -> PublishResult {
    let id = tx.mantle_tx.hash();
    state.submit_other(tx);
    *last_msg_id = msg_id;
    PublishResult { inscription_id: id }
}

fn build_checkpoint(state: &TxState, last_msg_id: MsgId, lib_slot: Slot) -> SequencerCheckpoint {
    SequencerCheckpoint {
        last_msg_id,
        pending_txs: state.all_pending_txs(),
        lib: state.lib(),
        lib_slot,
    }
}

fn restored_pending_channel_tip(
    pending_txs: &[(TxHash, SignedMantleTx)],
    channel_id: ChannelId,
) -> Option<MsgId> {
    let mut parents = Vec::new();
    let mut children = std::collections::HashSet::new();

    for (_, tx) in pending_txs {
        for op in tx.mantle_tx.ops() {
            if let Op::ChannelInscribe(ins) = op
                && ins.channel_id == channel_id
            {
                parents.push(ins.parent);
                children.insert(ins.id());
            }
        }
    }

    parents
        .into_iter()
        .find(|parent| !children.contains(parent))
}

/// Restore a single pending tx into `TxState` on checkpoint resume.
///
/// Inspects the tx ops:
/// - Any `Op::ChannelWithdraw` targeting our channel → bundle. Restored via
///   `submit_atomic_withdraw` so `PendingInscription.withdraws` is repopulated
///   and orphan/finalize emit the correct [`PublishedTx::AtomicWithdraw`] /
///   [`OrphanedTx::AtomicWithdraw`] variant.
/// - Only `Op::ChannelInscribe` for our channel → plain inscription.
/// - Neither → treated as opaque (`submit_other`).
///
/// Txs for other channels (checkpoint reused across channels) hit the
/// `submit_other` fallback.
///
/// A tx with 2+ `ChannelInscribe` ops for our channel (constructable via
/// `prepare_tx` + `submit_signed_tx`) isn't a bundle our API can represent.
/// We log an error and fall back to `submit_other` — the tx is still tracked
/// for finalize/orphan, just without per-tx inscription lineage.
fn restore_pending_tx(state: &mut TxState, tx: SignedMantleTx, channel_id: ChannelId) {
    let tx_hash = tx.mantle_tx.hash();
    let mut inscribe_meta: Option<(MsgId, MsgId, Inscription)> = None;
    let mut multi_inscribe = false;
    let mut withdraws: Vec<WithdrawInfo> = Vec::new();
    for op in tx.mantle_tx.ops() {
        match op {
            Op::ChannelInscribe(i) if i.channel_id == channel_id => {
                if inscribe_meta.is_some() {
                    multi_inscribe = true;
                } else {
                    inscribe_meta = Some((i.parent, i.id(), i.inscription.clone()));
                }
            }
            Op::ChannelWithdraw(w) if w.channel_id == channel_id => {
                withdraws.push(WithdrawInfo {
                    tx_hash,
                    op: w.clone(),
                });
            }
            _ => {}
        }
    }
    if multi_inscribe {
        error!(
            tx_hash = %hex::encode(tx.mantle_tx.hash().0),
            "restore_pending_tx: tx has multiple ChannelInscribe ops for our channel; \
             tracking as opaque (no bundle lineage)"
        );
        state.submit_other(tx);
        return;
    }
    match inscribe_meta {
        Some((parent, this_msg, payload)) => {
            if withdraws.is_empty() {
                state.submit_inscription(tx, parent, this_msg, payload);
            } else {
                state.submit_atomic_withdraw(tx, parent, this_msg, payload, withdraws);
            }
        }
        None => state.submit_other(tx),
    }
}

/// Result of processing a block event.
struct BlockEventResult {
    /// Finalized channel txs in tx/op execution order across blocks. Each
    /// [`FinalizedTx`] groups all channel-relevant ops from a single Mantle
    /// tx — inscriptions (ours or others'), deposits (with `amount` from the
    /// chain events API) and withdraws (standalone or part of an atomic
    /// inscription+withdraw bundle).
    finalized_items: Vec<FinalizedTx>,
    channel_update: Option<crate::state::ChannelUpdateInfo>,
}

/// Process a block event. Returns finalized tx hashes and optional channel
/// update.
///
/// Returns [`Err`] if the LIB-range backfill (blocks or deposit events) fails
/// for this event. On error, `state`, `current_tip`, and `lib_slot` are left
/// untouched so the caller can drop the block stream and have the reconnect
/// path retry this same event.
async fn handle_block_event<Node>(
    event: &ProcessedBlockEvent,
    state: &mut Option<TxState>,
    current_tip: &mut Option<HeaderId>,
    lib_slot: &mut Slot,
    channel_id: ChannelId,
    node: &Node,
) -> Result<BlockEventResult, Error>
where
    Node: adapter::Node + Sync,
{
    let block_id = event.block.header.id;
    let parent_id = event.block.header.parent_block;
    let tip = event.tip;
    let lib = event.lib;

    // Initialize state on first event
    if state.is_none() {
        *state = Some(TxState::new(lib, MsgId::root()));
    }

    let Some(s) = state.as_mut() else {
        return Ok(BlockEventResult {
            finalized_items: Vec::new(),
            channel_update: None,
        });
    };

    let old_tip = *current_tip;

    // Backfill if needed (self-healing on every event)
    // 1. Backfill finalized blocks up to LIB (only when state's LIB is behind).
    //    Done BEFORE we advance `*lib_slot` and BEFORE we mutate state for the live
    //    event — so on a fetch failure the caller can retry the same event next
    //    time around.
    let mut lib_finalized = Vec::new();
    let mut finalized_items: Vec<FinalizedTx> = Vec::new();
    if lib != s.lib() {
        let new_lib_slot = event.lib_slot;
        let from: u64 = (*lib_slot).into();
        let to: u64 = new_lib_slot.into();
        if from < to {
            let batch = fetch_and_process_blocks(s, from + 1, to, channel_id, node).await?;
            lib_finalized = batch.our_tx_hashes;
            finalized_items = batch.items;
        }
        *lib_slot = new_lib_slot;
    }

    // 2. Backfill canonical chain if parent is missing
    if !s.has_block(&parent_id) && parent_id != s.lib() {
        backfill_canonical(s, parent_id, channel_id, node).await;
    }

    // Extract tx hashes and inscription info for our channel
    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| matches_channel(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    let inscriptions = extract_inscriptions(&event.block.transactions, channel_id);

    // Process the actual event block
    s.process_block(block_id, parent_id, lib, our_txs, inscriptions);

    // Remove our pending txs that were finalized in the backfilled LIB blocks.
    // `finalized_items` already carries the typed payloads (built before
    // pending was mutated) so we just need to clean up state here.
    for tx_hash in &lib_finalized {
        s.remove_pending(tx_hash);
    }
    *current_tip = Some(tip);

    // Detect channel changes.
    // On first event (old_tip is None), check for existing inscriptions on
    // the channel — this handles clean start on an existing channel.
    // On subsequent events, detect channel update if tip changed.
    let channel_update = match old_tip {
        Some(old) if old != tip => s.detect_channel_update(old, tip),
        None => {
            // First event — no old canonical exists yet, so nothing can be
            // orphaned. Report any inscriptions on the initial tip as adopted.
            let channel_tip = s.channel_tip_at(tip);
            if channel_tip == MsgId::root() {
                None
            } else {
                let adopted = s.collect_inscriptions_on_branch(tip);
                (!adopted.is_empty()).then_some(crate::state::ChannelUpdateInfo {
                    orphaned: Vec::new(),
                    adopted,
                    new_channel_tip: channel_tip,
                })
            }
        }
        _ => None, // tip unchanged
    };

    Ok(BlockEventResult {
        finalized_items,
        channel_update,
    })
}

/// Convert a shed pending entry into an [`OrphanedTx`] for surfacing to the
/// consumer. Pending only ever contains inscription / atomic-withdraw
/// variants — the [`PublishedTx::Deposit`] case is unreachable in practice
/// (sequencers never publish deposits) and is logged + skipped.
fn orphan_from_shed(entry: PublishedTx) -> Option<OrphanedTx> {
    if let Some(info) = entry.inscription() {
        debug!(
            target: TARGET,
            "  orphaned: payload={:?}, tx={}, msg_id={}",
            String::from_utf8_lossy(&info.payload),
            hex::encode(info.tx_hash.0),
            hex::encode(info.this_msg.as_ref()),
        );
    }
    match entry {
        PublishedTx::Inscription(i) => Some(OrphanedTx::Inscription(i)),
        PublishedTx::AtomicWithdraw(a) => Some(OrphanedTx::AtomicWithdraw(a)),
        PublishedTx::Deposit(_) => {
            debug!("  orphaned: unexpected Deposit entry in pending; skipping");
            None
        }
    }
}

/// Result of fetching and processing a slot range.
struct FetchedBatch {
    /// Tx hashes of txs that match our channel (any op). Used internally to
    /// clean up our pending set.
    our_tx_hashes: Vec<TxHash>,
    /// User-facing finalized txs, one entry per channel-relevant Mantle tx,
    /// in block then tx order across the range. Each entry carries its ops
    /// in on-chain execution order.
    items: Vec<FinalizedTx>,
}

/// Fetch blocks in a slot range, process them into state, and return our
/// finalized tx hashes plus the user-facing items grouped per Mantle tx.
///
/// State is mutated only after the per-block fetch (blocks + events) has
/// fully succeeded. On any failure the function returns [`Err`] without
/// having advanced `state` for the failing block (earlier blocks in the
/// range are kept — they were independent successful units of work). The
/// caller is expected to abandon the current attempt and retry the range
/// later; the partial advance ensures progress on transient errors that
/// resolve mid-range.
async fn fetch_and_process_blocks<Node>(
    state: &mut TxState,
    from_slot: u64,
    to_slot: u64,
    channel_id: ChannelId,
    node: &Node,
) -> Result<FetchedBatch, Error>
where
    Node: adapter::Node + Sync,
{
    let mut result = FetchedBatch {
        our_tx_hashes: Vec::new(),
        items: Vec::new(),
    };

    let blocks = node
        .immutable_blocks(Slot::from(from_slot), Slot::from(to_slot))
        .await
        .map_err(|e| {
            error!(?from_slot, ?to_slot, ?e, "Failed to fetch immutable blocks");
            Error::Network(format!(
                "failed to fetch blocks (slots {from_slot}..{to_slot}): {e}"
            ))
        })?;

    for block in blocks {
        let our_txs: Vec<TxHash> = block
            .transactions
            .iter()
            .filter(|tx| matches_channel(tx, channel_id))
            .map(|tx| tx.mantle_tx.hash())
            .collect();

        let inscriptions = extract_inscriptions(&block.transactions, channel_id);

        // Fetch + validate deposit events for this block BEFORE mutating
        // state — on error we leave state untouched so the caller can retry.
        let deposit_amounts =
            fetch_block_deposit_amounts(node, block.header.id, &block.transactions, channel_id)
                .await?;
        let block_items =
            extract_finalized_items(&block.transactions, channel_id, &deposit_amounts);

        result.our_tx_hashes.extend(our_txs.iter().copied());
        result.items.extend(block_items);

        let current_lib = state.lib();
        state.process_block(
            block.header.id,
            block.header.parent_block,
            current_lib,
            our_txs,
            inscriptions,
        );
    }

    Ok(result)
}

/// Fetch the deposit-amount lookup for a single block, gated on whether the
/// block has any deposit op for our channel.
///
/// Per node semantics, a block and its events are atomically visible — so a
/// block containing a deposit op must yield an event for that op. The
/// returned `HashMap` is therefore the *complete* `(tx_hash, op_id) → amount`
/// lookup for every deposit op of our channel in this block.
///
/// On any failure (HTTP error, `Ok(None)`, or events missing an entry for
/// some deposit op) we log at error level and return [`Error::Network`]. The
/// caller's contract is "either retry, or abandon this block" — never
/// silently emit a partial result, because that drops real deposits.
async fn fetch_block_deposit_amounts<Node>(
    node: &Node,
    block_id: HeaderId,
    transactions: &[SignedMantleTx],
    channel_id: ChannelId,
) -> Result<HashMap<(TxHash, Hash), Value>, Error>
where
    Node: adapter::Node + Sync,
{
    let expected: Vec<(TxHash, Hash)> = transactions
        .iter()
        .flat_map(|tx| {
            let tx_hash = tx.mantle_tx.hash();
            tx.mantle_tx.ops().iter().filter_map(move |op| match op {
                Op::ChannelDeposit(d) if d.channel_id == channel_id => Some((tx_hash, d.op_id())),
                _ => None,
            })
        })
        .collect();

    if expected.is_empty() {
        return Ok(HashMap::new());
    }

    let events = match node.block_events(block_id).await {
        Ok(Some(events)) => events,
        Ok(None) => {
            error!(
                ?block_id,
                "Events endpoint returned no body for a block with a channel deposit; \
                 events should be atomically visible with the block"
            );
            return Err(Error::Network(format!(
                "no events for block {block_id} containing channel deposits"
            )));
        }
        Err(err) => {
            error!(?block_id, ?err, "Failed to fetch events for block");
            return Err(Error::Network(format!(
                "failed to fetch events for block {block_id}: {err}"
            )));
        }
    };

    let amounts = build_deposit_amounts(&events);
    for key in &expected {
        if !amounts.contains_key(key) {
            error!(
                ?block_id,
                tx_hash = ?key.0,
                op_id = ?key.1,
                "Block events missing an entry for a known channel deposit op; \
                 expected atomic block/events visibility per node semantics"
            );
            return Err(Error::Network(format!(
                "block {block_id} events missing deposit entry for tx {:?} op {:?}",
                key.0, key.1
            )));
        }
    }
    Ok(amounts)
}

/// Walks `transactions` and groups channel-relevant ops per Mantle tx,
/// preserving on-chain execution order both across and within txs.
///
/// Each returned [`FinalizedTx`] corresponds to one Mantle tx that touched
/// our channel. Its `ops` are in op order: a tx with `Deposit + Inscribe`
/// emits `[Deposit, Inscribe]`. Atomicity is structural — every op inside
/// the same [`FinalizedTx`] succeeded together on chain.
///
/// The channel protocol guarantees a linear parent-child chain per channel
/// within a block, so tx order already equals parent-chain order — do NOT
/// add a topological sort here, it would mask any real protocol violation
/// rather than fix it.
///
/// Deposits without a matching event entry are skipped with a warning.
fn extract_finalized_items(
    transactions: &[SignedMantleTx],
    channel_id: ChannelId,
    deposit_amounts: &HashMap<(TxHash, Hash), Value>,
) -> Vec<FinalizedTx> {
    let mut items: Vec<FinalizedTx> = Vec::new();
    let mut last_in_block: Option<MsgId> = None;

    for tx in transactions {
        let tx_hash = tx.mantle_tx.hash();
        let mut ops: Vec<FinalizedOp> = Vec::new();
        for op in tx.mantle_tx.ops() {
            match op {
                Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                    let info = InscriptionInfo {
                        tx_hash,
                        parent_msg: inscribe.parent,
                        this_msg: inscribe.id(),
                        payload: inscribe.inscription.clone(),
                    };
                    last_in_block = Some(info.this_msg);
                    ops.push(FinalizedOp::Inscription(info));
                }
                Op::ChannelConfig(config) if config.channel == channel_id => {
                    // Synthetic entry — keeps `channel_tip` in sync when the
                    // chain `ChannelConfig` resets it. Empty payload so
                    // payload-keyed consumers ignore it naturally.
                    let parent_msg = last_in_block.unwrap_or_else(MsgId::root);
                    let info = InscriptionInfo {
                        tx_hash,
                        parent_msg,
                        this_msg: config.id(),
                        payload: Inscription::new_unchecked(Vec::new()),
                    };
                    last_in_block = Some(info.this_msg);
                    ops.push(FinalizedOp::Inscription(info));
                }
                Op::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
                    let op_id = deposit.op_id();
                    // `fetch_block_deposit_amounts` validates that every
                    // channel-deposit op in the block has a matching event
                    // entry before returning, so the lookup is infallible
                    // here. A miss would be a caller-side bug.
                    let &amount = deposit_amounts.get(&(tx_hash, op_id)).expect(
                        "deposit_amounts must contain every channel deposit op - \
                         fetch_block_deposit_amounts invariant",
                    );
                    ops.push(FinalizedOp::Deposit(DepositInfo {
                        tx_hash,
                        op_id,
                        channel_id,
                        inputs: deposit.inputs.clone(),
                        amount,
                        metadata: deposit.metadata.clone(),
                    }));
                }
                Op::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
                    ops.push(FinalizedOp::Withdraw(WithdrawInfo {
                        tx_hash,
                        op: withdraw.clone(),
                    }));
                }
                _ => {}
            }
        }
        if !ops.is_empty() {
            items.push(FinalizedTx { tx_hash, ops });
        }
    }

    items
}

/// Backfill canonical chain backwards from a missing parent to LIB.
///
/// Uses `state.lib()` during replay to avoid premature finalization.
/// The caller is responsible for triggering finalization after backfill
/// completes.
async fn backfill_canonical<Node>(
    state: &mut TxState,
    missing_parent: HeaderId,
    channel_id: ChannelId,
    node: &Node,
) where
    Node: adapter::Node + Sync,
{
    debug!("Backfilling canonical chain from {:?}", missing_parent);
    let blocks = walk_back_to_known(state, missing_parent, node).await;
    let lib = state.lib();
    for block in &blocks {
        apply_backfilled_block(state, block, channel_id, lib);
    }
    debug!("Canonical backfill complete");
}

/// Walk backwards from `from` until a block the state already knows about (or
/// LIB) is reached. Returns blocks in forward order (oldest first).
async fn walk_back_to_known<Node>(
    state: &TxState,
    from: HeaderId,
    node: &Node,
) -> Vec<lb_common_http_client::ApiBlock>
where
    Node: adapter::Node + Sync,
{
    let mut blocks = Vec::new();
    let mut current = from;
    let lib = state.lib();

    while !state.has_block(&current) && current != lib {
        match node.block(current).await {
            Ok(Some(block)) => {
                let parent = block.header.parent_block;
                blocks.push(block);
                current = parent;
            }
            Ok(None) => {
                warn!("Block {:?} not found during canonical backfill", current);
                break;
            }
            Err(e) => {
                warn!(
                    "Failed to fetch block {:?} during canonical backfill: {e}",
                    current
                );
                break;
            }
        }
    }

    blocks.reverse();
    blocks
}

fn apply_backfilled_block(
    state: &mut TxState,
    block: &lb_common_http_client::ApiBlock,
    channel_id: ChannelId,
    lib: HeaderId,
) {
    let block_id = block.header.id;
    let parent_id = block.header.parent_block;

    let our_txs: Vec<TxHash> = block
        .transactions
        .iter()
        .filter(|tx| matches_channel(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    let inscriptions = extract_inscriptions(&block.transactions, channel_id);

    // Use current state lib to avoid premature finalization
    state.process_block(block_id, parent_id, lib, our_txs, inscriptions);
}

/// Extract channel inscription info from a block's transactions, in
/// parent→child chain order. Transactions in a block are not guaranteed
/// to be in chain order, so we topologically sort by inscription lineage.
/// Callers (e.g. `channel_tip_at`) rely on `last()` being the chain tail.
///
/// Panics if the inscriptions for the channel in a single block do not
/// form a single linear chain — that would be a protocol-level invariant
/// violation.
fn extract_inscriptions(txs: &[SignedMantleTx], channel_id: ChannelId) -> Vec<InscriptionInfo> {
    // Also tracks ChannelConfig as a synthetic tip-update entry so the SDK's
    // channel_tip stays in sync with the chain. Per spec, ChannelConfig sets
    // `chan.tip_hash = hash(encode(config))`, replacing whatever was there.
    // Synthetic entries have empty payload so app-layer consumers (which key
    // off payload bytes) ignore them naturally.
    let mut items: Vec<InscriptionInfo> = Vec::new();
    let mut last_in_block: Option<MsgId> = None;
    let hash_and_ops = txs
        .iter()
        .flat_map(|tx| std::iter::repeat(tx.mantle_tx.hash()).zip(tx.mantle_tx.ops().iter()));

    for (tx_hash, op) in hash_and_ops {
        match op {
            Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                let info = InscriptionInfo {
                    tx_hash,
                    parent_msg: inscribe.parent,
                    this_msg: inscribe.id(),
                    payload: inscribe.inscription.clone(),
                };
                last_in_block = Some(info.this_msg);
                items.push(info);
            }
            Op::ChannelConfig(config) if config.channel == channel_id => {
                // Chain off the previous in-block tip (or root) so the
                // topological sort below can stitch it into a single chain.
                let parent_msg = last_in_block.unwrap_or_else(MsgId::root);
                let info = InscriptionInfo {
                    tx_hash,
                    parent_msg,
                    this_msg: config.id(),
                    payload: [].into(),
                };
                last_in_block = Some(info.this_msg);
                items.push(info);
            }
            _ => {}
        }
    }

    if items.len() <= 1 {
        return items;
    }

    let this_msgs: std::collections::HashSet<MsgId> = items.iter().map(|i| i.this_msg).collect();
    let by_parent: HashMap<MsgId, &InscriptionInfo> =
        items.iter().map(|i| (i.parent_msg, i)).collect();

    // The chain root is the inscription whose parent is not produced
    // within this same block.
    let root = items
        .iter()
        .find(|i| !this_msgs.contains(&i.parent_msg))
        .expect("inscriptions for a channel in a block must form a chain (no root found)");

    let mut sorted = Vec::with_capacity(items.len());
    sorted.push(root.clone());
    let mut current = root.this_msg;
    while let Some(next) = by_parent.get(&current).copied() {
        sorted.push(next.clone());
        current = next.this_msg;
    }
    sorted
}

fn matches_channel(tx: &SignedMantleTx, channel_id: ChannelId) -> bool {
    tx.mantle_tx.ops().iter().any(|op| match op {
        Op::ChannelInscribe(inscribe) => inscribe.channel_id == channel_id,
        Op::ChannelConfig(set_keys) => set_keys.channel == channel_id,
        _ => false,
    })
}

fn create_inscribe_tx(
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> (SignedMantleTx, MsgId) {
    let signer = signing_key.public_key();

    let inscribe_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer,
    };
    let msg_id = inscribe_op.id();

    // TODO: set realistic gas prices and fund tx
    let inscribe_tx = MantleTx([Op::ChannelInscribe(inscribe_op)].into());

    let tx_hash = inscribe_tx.hash();
    let signature = sign_tx(tx_hash, signing_key);

    let signed_tx = SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        mantle_tx: inscribe_tx,
    };

    (signed_tx, msg_id)
}

fn create_channel_config_tx(
    channel_id: ChannelId,
    signing_keys: &[&Ed25519Key],
    keys: Keys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
    withdraw_threshold: u16,
) -> SignedMantleTx {
    let config_op = ChannelConfigOp {
        channel: channel_id,
        keys,
        posting_timeframe,
        posting_timeout,
        configuration_threshold,
        withdraw_threshold,
    };

    // TODO: fund tx
    let config_tx = MantleTx([Op::ChannelConfig(config_op)].into());

    let tx_hash = config_tx.hash();
    let signatures = signing_keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            IndexedSignature::new(
                index as ChannelKeyIndex,
                key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )
        })
        .collect();
    let proof = ChannelMultiSigProof::new(signatures).unwrap();

    SignedMantleTx {
        ops_proofs: vec![OpProof::ChannelMultiSigProof(proof)],
        mantle_tx: config_tx,
    }
}

fn prepare_tx(
    mut ops: Ops,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> (MantleTx, MsgId, Ed25519Signature) {
    let inscription_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer: signing_key.public_key(),
    };
    let msg_id = inscription_op.id();
    // TODO: Return `Error` in case there's too many ops already.
    ops.try_push(Op::ChannelInscribe(inscription_op)).unwrap();

    // TODO: fund tx
    let tx = MantleTx(ops);

    let inscription_sig = sign_tx(tx.hash(), signing_key);

    (tx, msg_id, inscription_sig)
}

fn sign_tx(tx_hash: TxHash, signing_key: &Ed25519Key) -> Ed25519Signature {
    signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use lb_common_http_client::{
        ApiBlock, ApiHeader, BlockInfo, ChainServiceMode, CryptarchiaInfo, State,
    };
    use lb_core::{
        header::ContentId,
        mantle::{
            Note, Utxo,
            ledger::Inputs,
            ops::channel::deposit::{DepositOp, Metadata},
        },
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_http_api_common::queries::BlocksStreamQuery;
    use lb_key_management_system_service::keys::ZkKey;
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};

    use super::*;
    use crate::ZoneMessage;

    #[must_use]
    pub fn utxo_with_sk() -> (ZkKey, Utxo) {
        let mut op_id = [0u8; 32];
        thread_rng().fill_bytes(&mut op_id);
        let zk_sk = ZkKey::from(BigUint::from(0u64));
        let utxo = Utxo {
            op_id,
            output_index: 0,
            note: Note::new(10, zk_sk.to_public_key()),
        };

        (zk_sk, utxo)
    }

    #[tokio::test]
    async fn prepare_submit_deposit_and_inscription() {
        // Init a sequencer
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (node, mut posted_txs) = MockNode::new();
        let (mut sequencer, handle) = ZoneSequencer::init(channel_id, sequencer_key, node, None);

        // Drive sequencer until ready
        loop {
            if matches!(
                sequencer.next_event().await,
                Some(Event::Readiness { ready: true })
            ) {
                break;
            }
        }

        // Prepare a deposit op
        let (sk, utxo) = utxo_with_sk();
        let deposit_op = DepositOp {
            channel_id,
            inputs: Inputs::new([utxo.id()]),
            metadata: b"to Alice".into(),
        };

        // Prepare a `MantleTx` — drive sequencer concurrently to process the request
        let prepare_fut = handle.prepare_tx(
            [Op::ChannelDeposit(deposit_op.clone())].into(),
            b"Mint 10 to Alice".into(),
        );
        tokio::pin!(prepare_fut);
        let (tx, msg_id, inscription_sig) = loop {
            tokio::select! {
                result = &mut prepare_fut => break result.unwrap(),
                _ = sequencer.next_event() => {}
            }
        };
        assert_eq!(tx.ops().len(), 2);
        assert_eq!(&tx.ops()[0], &Op::ChannelDeposit(deposit_op));
        assert!(matches!(&tx.ops()[1], &Op::ChannelInscribe(_)));

        // Sign the `MantleTx`
        let signed_tx = SignedMantleTx::new(
            tx.clone(),
            vec![
                OpProof::ZkSig(
                    ZkKey::multi_sign(std::slice::from_ref(&sk), &tx.clone().hash().to_fr())
                        .unwrap(),
                ),
                OpProof::Ed25519Sig(inscription_sig),
            ],
        )
        .unwrap();

        // Submit the signed tx — drive sequencer concurrently to process
        let submit_fut = handle.submit_signed_tx(signed_tx.clone(), msg_id);
        tokio::pin!(submit_fut);
        let result = loop {
            tokio::select! {
                result = &mut submit_fut => break result.unwrap(),
                _ = sequencer.next_event() => {}
            }
        };
        assert_eq!(result.inscription_id, signed_tx.mantle_tx.hash());
        assert_eq!(sequencer.checkpoint().unwrap().last_msg_id, msg_id);
        assert_eq!(posted_txs.recv().await.unwrap(), signed_tx);
    }

    /// Build a `SignedMantleTx` carrying the given ops, with placeholder
    /// proofs. Suitable for tests that only care about op extraction, not
    /// verification.
    fn unverified_tx_with_ops(ops: Vec<Op>) -> SignedMantleTx {
        let n = ops.len();
        let mantle_tx = MantleTx(Ops::try_from(ops).unwrap());
        SignedMantleTx::new_unverified(
            mantle_tx,
            vec![OpProof::Ed25519Sig(Ed25519Signature::zero()); n],
        )
    }

    fn deposit_op(channel_id: ChannelId, input_seed: u32, metadata: Metadata) -> DepositOp {
        use lb_core::mantle::NoteId;
        use lb_groth16::Fr;
        DepositOp {
            channel_id,
            inputs: Inputs::new([NoteId::from(Fr::from(input_seed))]),
            metadata,
        }
    }

    /// Extract deposits via the unified walker and filter to deposit entries
    /// for assertion clarity.
    fn extract_deposits_for_test(
        transactions: &[SignedMantleTx],
        channel_id: ChannelId,
        amounts: &HashMap<(TxHash, Hash), u64>,
    ) -> Vec<DepositInfo> {
        extract_finalized_items(transactions, channel_id, amounts)
            .into_iter()
            .flat_map(|t| t.ops.into_iter())
            .filter_map(|op| match op {
                FinalizedOp::Deposit(d) => Some(d),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn extract_deposits_returns_matching_amount() {
        let channel_id = ChannelId::from([0; 32]);
        let other_channel = ChannelId::from([1; 32]);

        let deposit_for_us = deposit_op(channel_id, 1, b"to Alice".into());
        let deposit_other_channel = deposit_op(other_channel, 2, b"to Bob".into());
        let our_op_id = deposit_for_us.op_id();

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelDeposit(deposit_for_us.clone()),
            Op::ChannelDeposit(deposit_other_channel),
        ]);
        let tx_hash = tx.mantle_tx.hash();

        let mut amounts = HashMap::new();
        amounts.insert((tx_hash, our_op_id), 1234u64);

        let deposits = extract_deposits_for_test(std::slice::from_ref(&tx), channel_id, &amounts);
        assert_eq!(
            deposits.len(),
            1,
            "only deposit on our channel is extracted"
        );
        let d = &deposits[0];
        assert_eq!(d.channel_id, channel_id);
        assert_eq!(d.tx_hash, tx_hash);
        assert_eq!(d.op_id, our_op_id);
        assert_eq!(d.amount, 1234);
        assert_eq!(d.metadata, b"to Alice".into());
        assert_eq!(d.inputs, deposit_for_us.inputs);
    }

    #[test]
    #[should_panic(expected = "fetch_block_deposit_amounts invariant")]
    fn extract_finalized_items_panics_if_deposit_amounts_incomplete() {
        // The walker contract: `deposit_amounts` must contain an entry for
        // every channel-deposit op in the input transactions. This is
        // enforced upstream by `fetch_block_deposit_amounts`, which validates
        // completeness and errors out before the walker is ever called with a
        // gap. A panic here surfaces the bug immediately if a future caller
        // violates that invariant — silent skip would drop a real deposit.
        let channel_id = ChannelId::from([0; 32]);
        let op = deposit_op(channel_id, 1, b"to Alice".into());
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(op)]);
        drop(extract_finalized_items(
            std::slice::from_ref(&tx),
            channel_id,
            &HashMap::new(),
        ));
    }

    #[test]
    fn extract_deposits_preserves_tx_and_op_order() {
        let channel_id = ChannelId::from([0; 32]);
        let d1 = deposit_op(channel_id, 1, b"first".into());
        let d2 = deposit_op(channel_id, 2, b"second".into());
        let d3 = deposit_op(channel_id, 3, b"third".into());
        let id1 = d1.op_id();
        let id2 = d2.op_id();
        let id3 = d3.op_id();

        // tx_a carries d1 then d2 (in op order); tx_b carries d3.
        let tx_a = unverified_tx_with_ops(vec![Op::ChannelDeposit(d1), Op::ChannelDeposit(d2)]);
        let tx_b = unverified_tx_with_ops(vec![Op::ChannelDeposit(d3)]);
        let hash_a = tx_a.mantle_tx.hash();
        let hash_b = tx_b.mantle_tx.hash();

        let mut amounts = HashMap::new();
        amounts.insert((hash_a, id1), 10);
        amounts.insert((hash_a, id2), 20);
        amounts.insert((hash_b, id3), 30);

        let deposits = extract_deposits_for_test(&[tx_a, tx_b], channel_id, &amounts);
        let metadata_in_order: Vec<&[u8]> =
            deposits.iter().map(|d| d.metadata.as_slice()).collect();
        assert_eq!(
            metadata_in_order,
            vec![b"first" as &[u8], b"second", b"third"],
            "deposits emitted in tx/op order across transactions"
        );
    }

    #[test]
    fn extract_finalized_items_interleaves_deposit_then_inscription_in_same_tx() {
        // The atomic deposit+inscription pattern: one Mantle tx with
        // [ChannelDeposit, ChannelInscribe]. The bridge use case requires the
        // deposit to be emitted BEFORE the inscription so that consumers
        // (e.g. LEZ) can validate references from the inscription back to
        // the just-finalized deposit.
        let channel_id = ChannelId::from([0; 32]);
        let dep = deposit_op(channel_id, 1, b"deposit-meta".into());
        let dep_op_id = dep.op_id();
        let inscribe = InscriptionOp {
            channel_id,
            parent: MsgId::root(),
            inscription: Inscription::new_unchecked(Vec::new()),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };

        let tx =
            unverified_tx_with_ops(vec![Op::ChannelDeposit(dep), Op::ChannelInscribe(inscribe)]);
        let tx_hash = tx.mantle_tx.hash();

        let mut amounts = HashMap::new();
        amounts.insert((tx_hash, dep_op_id), 500u64);

        let items = extract_finalized_items(std::slice::from_ref(&tx), channel_id, &amounts);

        assert_eq!(items.len(), 1, "one FinalizedTx for the single Mantle tx");
        assert_eq!(items[0].tx_hash, tx_hash);
        assert_eq!(items[0].ops.len(), 2);
        assert!(matches!(items[0].ops[0], FinalizedOp::Deposit(_)));
        assert!(matches!(items[0].ops[1], FinalizedOp::Inscription(_)));
    }

    #[test]
    fn extract_finalized_items_surfaces_standalone_withdraw() {
        // A ChannelWithdraw not bundled with an inscription (e.g. from
        // another sequencer or future multi-sig) should still surface as
        // a FinalizedOp::Withdraw — the sequencer stream is the complete
        // finalized view, not a "what we tracked locally" view.
        let channel_id = ChannelId::from([0; 32]);
        let other_channel = ChannelId::from([9; 32]);
        let outputs = Outputs::new([Note::new(
            42,
            ZkKey::from(BigUint::from(0u64)).to_public_key(),
        )]);
        let withdraw_for_us = ChannelWithdrawOp {
            channel_id,
            outputs: outputs.clone(),
            withdraw_nonce: 7,
        };
        let withdraw_other = ChannelWithdrawOp {
            channel_id: other_channel,
            outputs,
            withdraw_nonce: 0,
        };

        let tx = unverified_tx_with_ops(vec![
            Op::ChannelWithdraw(withdraw_for_us),
            Op::ChannelWithdraw(withdraw_other),
        ]);
        let tx_hash = tx.mantle_tx.hash();

        let items = extract_finalized_items(std::slice::from_ref(&tx), channel_id, &HashMap::new());

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tx_hash, tx_hash);
        assert_eq!(items[0].ops.len(), 1, "only our channel's withdraw");
        match &items[0].ops[0] {
            FinalizedOp::Withdraw(w) => {
                assert_eq!(w.tx_hash, tx_hash);
                assert_eq!(w.op.channel_id, channel_id);
                assert_eq!(w.op.withdraw_nonce, 7);
            }
            other => panic!("expected Withdraw, got {other:?}"),
        }
    }

    #[derive(Clone)]
    struct MockNode {
        posted_transactions_sender: mpsc::Sender<SignedMantleTx>,
        channel_state: Option<ChannelState>,
    }

    impl MockNode {
        fn new() -> (Self, mpsc::Receiver<SignedMantleTx>) {
            let (tx, rx) = mpsc::channel(10);
            (
                Self {
                    posted_transactions_sender: tx,
                    channel_state: Some(ChannelState {
                        accredited_keys: Keys::from(Ed25519Key::from_bytes(&[0; 32]).public_key())
                            .into(),
                        configuration_threshold: 1,
                        tip_message: MsgId::root(),
                        tip_slot: Slot::default(),
                        tip_sequencer: 0,
                        tip_sequencer_starting_slot: Slot::default(),
                        posting_timeframe: 0u32.into(),
                        posting_timeout: 0u32.into(),
                        balance: 0,
                        withdrawal_nonce: 0,
                        withdraw_threshold: 1,
                    }),
                },
                rx,
            )
        }
    }

    #[test]
    fn restore_pending_tx_classifies_atomic_bundle_with_withdraws() {
        // Bundle: [ChannelWithdraw(channel_id), ChannelInscribe(channel_id)]
        // Restore should put it in pending (not pending_other) with the
        // withdraws field populated, so on orphan we emit
        // OrphanedTx::AtomicWithdraw (not Inscription).
        let channel_id = ChannelId::from([1u8; 32]);
        let outputs = Outputs::new([Note::new(
            5,
            ZkKey::from(BigUint::from(0u64)).to_public_key(),
        )]);
        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            outputs,
            withdraw_nonce: 0,
        };
        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };
        let mantle_tx = MantleTx(
            Ops::try_from(vec![
                Op::ChannelWithdraw(withdraw_op.clone()),
                Op::ChannelInscribe(inscribe_op),
            ])
            .unwrap(),
        );
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: Vec::new(),
        };

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        restore_pending_tx(&mut state, signed_tx, channel_id);

        let pending = state
            .pending_inscription(&tx_hash)
            .expect("bundle should be in pending inscriptions");
        let withdraws = pending
            .withdraws
            .as_ref()
            .expect("bundle should carry Some(withdraws)");
        assert_eq!(withdraws.len(), 1, "bundle should carry one WithdrawInfo");
        assert_eq!(withdraws[0].op, withdraw_op);
        assert!(
            !state.pending_other_contains(&tx_hash),
            "bundle should not be in pending_other"
        );
    }

    #[test]
    fn restore_pending_tx_classifies_plain_inscription_with_none_withdraws() {
        // Plain inscription: pending with `withdraws == None`.
        let channel_id = ChannelId::from([2u8; 32]);
        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };
        let mantle_tx = MantleTx(Ops::try_from(vec![Op::ChannelInscribe(inscribe_op)]).unwrap());
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: Vec::new(),
        };

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        restore_pending_tx(&mut state, signed_tx, channel_id);

        let pending = state
            .pending_inscription(&tx_hash)
            .expect("plain inscription should be in pending inscriptions");
        assert!(pending.withdraws.is_none());
    }

    #[test]
    fn restore_pending_tx_falls_back_to_other_when_no_inscribe_for_channel() {
        // Inscribe for a different channel: should fall back to pending_other
        // (treated as opaque).
        let our_channel = ChannelId::from([3u8; 32]);
        let other_channel = ChannelId::from([4u8; 32]);
        let inscribe_op = InscriptionOp {
            channel_id: other_channel,
            inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        };
        let mantle_tx = MantleTx(Ops::try_from(vec![Op::ChannelInscribe(inscribe_op)]).unwrap());
        let tx_hash = mantle_tx.hash();
        let signed_tx = SignedMantleTx {
            mantle_tx,
            ops_proofs: Vec::new(),
        };

        let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
        restore_pending_tx(&mut state, signed_tx, our_channel);

        assert!(
            state.pending_inscription(&tx_hash).is_none(),
            "wrong-channel tx should not be in pending inscriptions"
        );
        assert!(
            state.pending_other_contains(&tx_hash),
            "wrong-channel tx should be in pending_other"
        );
    }

    #[async_trait]
    impl adapter::Node for MockNode {
        async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
            Ok(ChainServiceInfo {
                cryptarchia_info: CryptarchiaInfo {
                    lib: HeaderId::from([0; 32]),
                    lib_slot: Slot::genesis(),
                    tip: HeaderId::from([0; 32]),
                    slot: Slot::genesis(),
                    height: 0,
                },
                mode: ChainServiceMode::Started(State::Online),
            })
        }

        async fn channel_state(
            &self,
            _channel_id: ChannelId,
        ) -> Result<Option<ChannelState>, lb_common_http_client::Error> {
            Ok(self.channel_state.clone())
        }

        async fn block_stream(
            &self,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(
                futures::stream::once(async {
                    ProcessedBlockEvent {
                        block: ApiBlock {
                            header: ApiHeader {
                                id: HeaderId::from([1; 32]),
                                parent_block: HeaderId::from([0; 32]),
                                slot: 1.into(),
                                block_root: ContentId::from([0; 32]),
                                proof_of_leadership: Groth16LeaderProof::genesis(),
                            },
                            transactions: Vec::new(),
                        },
                        tip: HeaderId::from([1; 32]),
                        tip_slot: 1.into(),
                        lib: HeaderId::from([0; 32]),
                        lib_slot: Slot::genesis(),
                    }
                })
                .chain(futures::stream::pending()),
            ))
        }

        async fn blocks_range_stream(
            &self,
            _params: BlocksStreamQuery,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn block_events(
            &self,
            _id: HeaderId,
        ) -> Result<Option<lb_common_http_client::Events>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn immutable_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
            Ok(Vec::new())
        }

        async fn zone_messages_in_block(
            &self,
            _id: HeaderId,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn zone_messages_in_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn post_transaction(
            &self,
            tx: SignedMantleTx,
        ) -> Result<(), lb_common_http_client::Error> {
            self.posted_transactions_sender.send(tx).await.unwrap();
            Ok(())
        }
    }

    /// Mock node that serves a single genesis-slot block with a channel
    /// inscription, used to verify the cold-start backfill picks up slot 0.
    #[derive(Clone)]
    struct ColdStartMockNode {
        genesis_block: ApiBlock,
        live_block: ApiBlock,
        channel_state: Option<ChannelState>,
    }

    #[async_trait]
    impl adapter::Node for ColdStartMockNode {
        async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
            Ok(ChainServiceInfo {
                cryptarchia_info: CryptarchiaInfo {
                    lib: self.genesis_block.header.id,
                    lib_slot: Slot::genesis(),
                    tip: self.genesis_block.header.id,
                    slot: Slot::genesis(),
                    height: 0,
                },
                mode: ChainServiceMode::Started(State::Online),
            })
        }

        async fn block_stream(
            &self,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            let block = self.live_block.clone();
            let genesis_id = self.genesis_block.header.id;
            Ok(Box::pin(
                futures::stream::once(async move {
                    ProcessedBlockEvent {
                        block,
                        tip: HeaderId::from([2; 32]),
                        tip_slot: 1.into(),
                        lib: genesis_id,
                        lib_slot: Slot::genesis(),
                    }
                })
                .chain(futures::stream::pending()),
            ))
        }

        async fn blocks_range_stream(
            &self,
            _params: BlocksStreamQuery,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn block_events(
            &self,
            _id: HeaderId,
        ) -> Result<Option<lb_common_http_client::Events>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn immutable_blocks(
            &self,
            slot_from: Slot,
            slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
            // Cold-start backfill range is [0, 0] when lib_slot is genesis,
            // so we only return the genesis block for that exact range.
            if slot_from == Slot::genesis() && slot_to == Slot::genesis() {
                Ok(vec![self.genesis_block.clone()])
            } else {
                Ok(Vec::new())
            }
        }

        async fn zone_messages_in_block(
            &self,
            _id: HeaderId,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn zone_messages_in_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn post_transaction(
            &self,
            _tx: SignedMantleTx,
        ) -> Result<(), lb_common_http_client::Error> {
            Ok(())
        }

        async fn channel_state(
            &self,
            _channel_id: ChannelId,
        ) -> Result<Option<ChannelState>, lb_common_http_client::Error> {
            Ok(self.channel_state.clone())
        }
    }

    /// Cold start with a channel inscription at slot 0 (genesis): the
    /// sequencer must include that slot in its initial backfill and emit it
    /// in `Event::TxsFinalized`. Regression guard for the off-by-one fix
    /// where `backfill_from = lib_slot + 1` silently skipped genesis.
    #[tokio::test]
    async fn cold_start_backfills_genesis_slot() {
        let channel_id = ChannelId::from([7; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

        // A signed tx with a single ChannelInscribe on our channel at
        // genesis (parent_msg = root).
        let inscribe = InscriptionOp {
            channel_id,
            parent: MsgId::root(),
            inscription: Inscription::new_unchecked(Vec::new()),
            signer: sequencer_key.public_key(),
        };
        let expected_msg_id = inscribe.id();
        let genesis_tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(inscribe)]);
        let genesis_tx_hash = genesis_tx.mantle_tx.hash();

        let genesis_block = ApiBlock {
            header: ApiHeader {
                id: HeaderId::from([1; 32]),
                parent_block: HeaderId::from([0; 32]),
                slot: Slot::genesis(),
                block_root: ContentId::from([0; 32]),
                proof_of_leadership: Groth16LeaderProof::genesis(),
            },
            transactions: vec![genesis_tx],
        };
        // Empty block at slot 1 so the block stream advances and the
        // sequencer signals `Ready`, giving the test a clean exit signal.
        let live_block = ApiBlock {
            header: ApiHeader {
                id: HeaderId::from([2; 32]),
                parent_block: HeaderId::from([1; 32]),
                slot: 1.into(),
                block_root: ContentId::from([0; 32]),
                proof_of_leadership: Groth16LeaderProof::genesis(),
            },
            transactions: Vec::new(),
        };

        let channel_state = Some(ChannelState {
            accredited_keys: Keys::from(Ed25519Key::from_bytes(&[0; 32]).public_key()).into(),
            configuration_threshold: 1,
            tip_message: MsgId::root(),
            tip_slot: Slot::default(),
            tip_sequencer: 0,
            tip_sequencer_starting_slot: Slot::default(),
            posting_timeframe: 0u32.into(),
            posting_timeout: 0u32.into(),
            balance: 0,
            withdrawal_nonce: 0,
            withdraw_threshold: 1,
        });

        let node = ColdStartMockNode {
            genesis_block,
            live_block,
            channel_state,
        };
        let (mut sequencer, _handle) = ZoneSequencer::init(channel_id, sequencer_key, node, None);

        let mut finalized_items: Vec<FinalizedTx> = Vec::new();
        loop {
            match sequencer.next_event().await {
                Some(Event::Readiness { ready: true }) => break,
                Some(Event::TxsFinalized { items }) => finalized_items.extend(items),
                Some(_) | None => {}
            }
        }

        assert_eq!(
            finalized_items.len(),
            1,
            "expected exactly one finalized tx from genesis backfill"
        );
        let t = &finalized_items[0];
        assert_eq!(t.tx_hash, genesis_tx_hash);
        assert_eq!(t.ops.len(), 1);
        match &t.ops[0] {
            FinalizedOp::Inscription(info) => {
                assert_eq!(info.tx_hash, genesis_tx_hash);
                assert_eq!(info.parent_msg, MsgId::root());
                assert_eq!(info.this_msg, expected_msg_id);
            }
            other => panic!("expected Inscription, got {other:?}"),
        }
    }
}
