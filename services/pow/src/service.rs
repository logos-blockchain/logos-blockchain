use core::fmt::{Debug, Display};
use std::{
    collections::{HashMap, HashSet},
    future::ready,
    marker::PhantomData,
    num::{NonZeroU64, NonZeroUsize},
    pin::Pin,
    sync::{Arc, LazyLock},
    time::Duration,
};

use futures::{Stream, StreamExt as _};
use lb_blend_service::{
    api::{ApiError as BlendApiError, BlendServiceApi, BlendServiceData},
    message::{BlendPayload, MAX_PAYLOAD_BODY_SIZE, TransactionTooLarge},
};
use lb_chain_service::{
    ProcessedBlockEvent, Slot,
    api::{ApiError as ChainApiError, CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    codec::{Error as CodecError, SerializeOp},
    events::{Event, TxEvent, TxEventPayload},
    header::HeaderId,
    mantle::{
        Note, NoteId, Op, OpProof, SignedOps, Utxo, Value,
        gas::MainnetGasProfile,
        ledger::{Inputs, InputsError, Outputs, verification_mode::StandardMode},
        ops::{
            NoOpProof, OpId as _,
            pow::{ClaimPowRewardOp, PowNullifier},
            transfer::TransferOp,
        },
        traits::Hashable as _,
        transactions::{
            GasPrices, MAX_OPS_PER_TX, MantleTxBuilder, OpProofs, TxBuilderError,
            hash::TxHash,
            states::Unverified,
            tx_list::ops::{OpsContext, OpsGasContext},
        },
    },
};
use lb_groth16::COMPRESSED_PROOF_SIZE;
use lb_key_management_system_keys::keys::{
    MAX_ZK_SIGNING_KEYS, UnsecuredZkKey, ZkPublicKey, ZkSignature,
};
use lb_ledger::LedgerState;
use lb_log_targets::pow;
use lb_services_utils::{
    overwatch::{RecoveryData, RecoveryOperator, StorageRecoverySettings},
    wait_until_services_are_ready,
};
use lb_storage_service::{
    StorageService, backends::StorageBackend, recovery::StorageRecoveryBackend,
};
use lb_time_service::{TimeService, TimeServiceMessage, backends::TimeBackend};
use lb_utils::bounded::BoundedError;
use lb_wallet_service::api::{WalletApi, WalletApiError, WalletServiceData};
use lb_zksign::{ZkSignError, ZkSignProof};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        relay::RelayError,
        state::{ServiceState, StateUpdater},
    },
};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::oneshot::{self, error::RecvError},
    task::JoinError,
};
use tokio_stream::wrappers::{BroadcastStream, IntervalStream, errors::BroadcastStreamRecvError};
use tracing::{
    error,
    log::{info, warn},
};

use crate::tickets::{TicketGenerator, WinningTicket};

const LOG_TARGET: &str = pow::ROOT;

/// Errors produced while building or publishing `PoW` reward-claim
/// transactions.
#[derive(thiserror::Error, Debug)]
pub enum PoWError {
    #[error("PoW rewards are disabled (epoch reward is zero)")]
    RewardsDisabled,
    #[error("no claims fit within the reward pool")]
    RewardPoolExhausted,
    #[error("reward value overflow")]
    RewardOverflow,
    #[error("PoW reward does not cover the transaction fee")]
    RewardBelowFee,
    #[error("failed to build transaction: {0}")]
    TxBuilder(#[from] TxBuilderError),
    #[error("invalid transfer inputs: {0}")]
    Inputs(#[from] InputsError),
    #[error("too many operation proofs: {0}")]
    OpProofs(#[from] BoundedError),
    #[error("failed to sign transfer: {0}")]
    Sign(#[from] ZkSignError),
    #[error("signing task failed: {0}")]
    SignTask(#[from] JoinError),
    #[error("failed to encode transaction: {0}")]
    Encode(#[from] CodecError),
    #[error("transaction too large for a blend payload: {0}")]
    PayloadTooLarge(#[from] TransactionTooLarge),
    #[error("failed to publish to the blend network: {0}")]
    Publish(#[from] BlendApiError),
    #[error("failed to query the chain service: {0}")]
    Chain(#[from] ChainApiError),
    #[error("failed to query the wallet service: {0}")]
    Wallet(#[from] WalletApiError),
    #[error("ledger state unavailable for tip {0:?}")]
    LedgerStateUnavailable(HeaderId),
    #[error("failed to reach the time service: {0}")]
    TimeRelay(RelayError),
    #[error("the time service dropped the slot-tick subscription response: {0}")]
    SlotTickSubscription(#[from] RecvError),
    #[error(
        "PoW auto-claim targets are not tracked by the wallet (add them to `wallet.known_keys`): \
         {0:?}"
    )]
    UntrackedClaimTargets(Vec<ZkPublicKey>),
    #[error("no claim address given and no auto-claim target is below its threshold")]
    NoClaimTarget,
    #[error("failed to build signed transaction: {0}")]
    SignedOps(#[from] lb_core::mantle::transactions::tx_list::signed_ops::Error),
}

/// Max inputs a single `Transfer` op can carry: its `ZkSig` is a
/// multi-signature over one key per input, capped at [`MAX_ZK_SIGNING_KEYS`].
/// Hence one transfer per 32 claims.
const MAX_TRANSFER_INPUTS: usize = MAX_ZK_SIGNING_KEYS;

/// A summary of the rewards this node can currently claim.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimableRewardsInfo {
    /// Number of mined tickets still within the reward window.
    pub claimable_tickets: usize,
    /// For each claimable ticket, how many more slots it stays within the
    /// reward window before it can no longer be claimed.
    pub slots_until_expiry: Vec<Slot>,
}

pub enum PoWServiceMessage {
    StartMining,
    StopMining,
    /// Re-arm the auto-claim ticker after it stopped itself (or was stopped).
    StartAutoClaim,
    /// Stop the auto-claim ticker. Manual claims keep working.
    StopAutoClaim,
    Claim {
        /// Key the claimed rewards are paid to. `None` falls back to the key
        /// auto-claim would pick right now (see [`select_claim_target`]).
        claim_address: Option<ZkPublicKey>,
        response: oneshot::Sender<Result<Option<TxHash>, PoWError>>,
    },
    ClaimableRewardsInfo {
        response: oneshot::Sender<ClaimableRewardsInfo>,
    },
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PoWServiceSettings {
    /// Tuning for the CPU-heavy ticket search (thread pool and per-block
    /// concurrency).
    #[serde(default)]
    pub mining: PoWMiningSettings,
    /// Unattended claiming: which keys to pay and how often to try. Omitting
    /// it leaves auto-claim off, so rewards are only claimed on demand.
    #[serde(default)]
    pub auto_claim: AutoClaimSettings,
    /// Acceptance window, in slots, a mined ticket stays claimable for. Must
    /// match the network's consensus `slot_window` (sourced from the same
    /// deployment configuration); a ticket outside it can never be claimed.
    pub slot_window: NonZeroU64,
    /// Storage-recovery bookkeeping, populated by the runtime on startup.
    #[serde(skip)]
    pub recovery_data: RecoveryData,
}

/// One auto-claim destination: a key and the balance we want it to reach.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ClaimTarget {
    /// Key the rewards are paid to. It must be one of the wallet's
    /// `known_keys`, or the node refuses to start (see
    /// [`validate_claim_targets`]).
    pub public_key: ZkPublicKey,
    /// Balance, in tokens, this key should reach. Once its on-chain balance is
    /// at or above this, the target is satisfied and no longer paid.
    pub threshold: Value,
}

/// How often the auto-claim ticker fires: on a wall-clock interval, or every
/// `n` slots of chain progress.
///
/// Adjacently tagged so it reads as `{ unit: seconds, value: 300 }` in both
/// YAML and JSON — `serde_yaml` only accepts an externally-tagged enum through
/// a `!Seconds`-style tag, which is a poor fit for a configuration file.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(tag = "unit", content = "value", rename_all = "snake_case")]
pub enum AutoClaimTick {
    Seconds(NonZeroU64),
    Slots(NonZeroU64),
}

/// Default auto-claim period: five minutes of wall-clock time.
const fn default_auto_claim_tick() -> AutoClaimTick {
    AutoClaimTick::Seconds(NonZeroU64::new(300).expect("300 is non-zero"))
}

impl Default for AutoClaimTick {
    fn default() -> Self {
        default_auto_claim_tick()
    }
}

/// Unattended claiming configuration.
///
/// On every tick the service pays the target holding the least value among
/// those still below their threshold, draining the ready tickets into it. With
/// no targets configured there is nothing to pay, so auto-claim stays off.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct AutoClaimSettings {
    /// Keys to pay, each with the balance it should reach. Empty disables
    /// auto-claim.
    #[serde(default)]
    pub targets: Vec<ClaimTarget>,
    /// Period between claim attempts.
    #[serde(default = "default_auto_claim_tick")]
    pub tick: AutoClaimTick,
}

/// Default number of ticket-search attempts kept in flight per block.
const fn default_max_tickets_per_block() -> NonZeroUsize {
    NonZeroUsize::new(4).expect("4 is non-zero")
}

/// Tuning knobs for the Proof-of-Work ticket search.
///
/// The search is CPU-bound and runs on a dedicated thread pool so it does not
/// starve Tokio's runtime threads. Both fields have sensible defaults, so an
/// omitted `mining` section keeps the previous behaviour. The non-zero types
/// make a `0` configuration a deserialization error rather than a silent stall.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PoWMiningSettings {
    /// Worker threads in the dedicated ticket-search pool. `None` lets rayon
    /// pick its default (one thread per logical CPU).
    #[serde(default)]
    pub max_threads: Option<NonZeroUsize>,
    /// Maximum ticket-search attempts kept in flight concurrently per block.
    #[serde(default = "default_max_tickets_per_block")]
    pub max_tickets_per_block: NonZeroUsize,
}

impl Default for PoWMiningSettings {
    fn default() -> Self {
        Self {
            max_threads: None,
            max_tickets_per_block: default_max_tickets_per_block(),
        }
    }
}

/// Builds the dedicated ticket-search thread pool.
///
/// Kept as a plain (non-async) helper so the non-`Send`
/// [`rayon::ThreadPoolBuilder`] never becomes part of the service's async
/// state. `max_threads == None` lets rayon default to one thread per logical
/// CPU.
fn build_search_pool(max_threads: Option<NonZeroUsize>) -> Arc<rayon::ThreadPool> {
    let mut builder = rayon::ThreadPoolBuilder::new();
    builder = builder.thread_name(|index| format!("logos/pow/pow-ticket-search-{index}"));
    if let Some(threads) = max_threads {
        builder = builder.num_threads(threads.get());
    }
    Arc::new(
        builder
            .build()
            .expect("PoW ticket search thread pool should build"),
    )
}

impl StorageRecoverySettings for PoWServiceSettings {
    const RECOVERY_KEY_SUFFIX: &'static [u8] = b"pow";

    fn recovery_data(&self) -> &RecoveryData {
        &self.recovery_data
    }
}

/// Persisted state of the `PoW` service: the winning tickets awaiting a claim.
///
/// It is persisted so the tickets survive restarts. Expired tickets are pruned
/// in place whenever the current slot is known (see [`prune_expired_tickets`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PoWServiceState {
    /// Mined tickets not yet submitted, drained into a claim transaction on a
    /// [`PoWServiceMessage::Claim`].
    ready_to_claim: Vec<WinningTicket>,
    /// Tickets whose claim transaction has been published but not yet observed
    /// as settled; retained until their reward window closes.
    pending_to_claim: Vec<WinningTicket>,
}

impl ServiceState for PoWServiceState {
    type Settings = PoWServiceSettings;
    type Error = core::convert::Infallible;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self::default())
    }
}

pub struct PoWService<
    CryptarchiaService,
    BlendService,
    WalletService,
    TimeBackendType,
    Storage,
    RuntimeServiceId,
> where
    Storage: StorageBackend + Send + Sync + 'static,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    state: PoWServiceState,
    settings: PoWServiceSettings,
    _phantom: PhantomData<(
        CryptarchiaService,
        BlendService,
        WalletService,
        TimeBackendType,
        Storage,
    )>,
}

impl<CryptarchiaService, BlendService, WalletService, TimeBackendType, Storage, RuntimeServiceId>
    ServiceData
    for PoWService<
        CryptarchiaService,
        BlendService,
        WalletService,
        TimeBackendType,
        Storage,
        RuntimeServiceId,
    >
where
    Storage: StorageBackend + Send + Sync + 'static,
{
    type Settings = PoWServiceSettings;
    type State = PoWServiceState;
    type StateOperator = RecoveryOperator<
        StorageRecoveryBackend<Self::State, Self::Settings, Storage, RuntimeServiceId>,
    >;
    type Message = PoWServiceMessage;
}

#[async_trait::async_trait]
impl<
    Tx,
    CryptarchiaService,
    BlendService,
    WalletService,
    TimeBackendType,
    Storage,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for PoWService<
        CryptarchiaService,
        BlendService,
        WalletService,
        TimeBackendType,
        Storage,
        RuntimeServiceId,
    >
where
    Tx: Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Tx> + Sync + 'static,
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    <BlendService as ServiceData>::Message: Send + 'static,
    WalletService: WalletServiceData + Send + Sync + 'static,
    <WalletService as ServiceData>::Message: Send + 'static,
    TimeBackendType: TimeBackend + Send + Sync + 'static,
    TimeBackendType::Settings: Send + Sync,
    Storage: StorageBackend + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Unpin
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<CryptarchiaService>
        + AsServiceId<BlendService>
        + AsServiceId<WalletService>
        + AsServiceId<TimeService<TimeBackendType, RuntimeServiceId>>
        + AsServiceId<StorageService<Storage, RuntimeServiceId>>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();
        Ok(Self {
            service_resources_handle,
            state: initial_state,
            settings,
            _phantom: PhantomData,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "The service run loop is cohesive and clearer kept in one place."
    )]
    async fn run(self) -> Result<(), DynError> {
        let Self {
            service_resources_handle,
            settings,
            mut state,
            _phantom,
        } = self;

        // The PoW service must not mine or claim until the chain is synced:
        // wait for the chain service to become ready and reach the
        // Online mode before starting.
        //
        // Every service this one talks to is awaited, because a relay only
        // connects — it does not guarantee the peer is serving its inbound
        // queue, so a message sent too early is simply never answered. Startup
        // itself sends two: the auto-claim targets are validated against the
        // wallet's known keys, and slot pacing subscribes to the time service's
        // slot clock. Blend is awaited on the same grounds, though it is only
        // used later, to publish claim transactions.
        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            None,
            CryptarchiaService,
            WalletService,
            TimeService<TimeBackendType, RuntimeServiceId>,
            BlendService
        )
        .await?;

        // API wrapper over the chain service relay, used to query chain state.
        let cryptarchia_api = CryptarchiaServiceApi::<CryptarchiaService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<CryptarchiaService>()
                .await
                .expect("Relay connection with Cryptarchia chain service should succeed"),
        );

        // Wait till chain is online to mine
        info!(target: LOG_TARGET, "Waiting for the chain to become online");
        cryptarchia_api.wait_until_chain_becomes_online().await?;
        info!(target: LOG_TARGET, "Chain is online; starting the PoW service");

        // API wrapper over the blend service relay, used to publish PoW reward
        // claim transactions to the blend network.
        let blend_api = BlendServiceApi::<BlendService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<BlendService>()
                .await
                .expect("Relay connection with BlendService should succeed"),
        );

        // API wrapper over the wallet service relay. Auto-claim reads each
        // target's balance through it to decide which key to pay next.
        let wallet_api = WalletApi::<WalletService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<WalletService>()
                .await
                .expect("Relay connection with WalletService should succeed"),
        );

        // A target the wallet does not track reports no balance, so its
        // threshold could never be observed as reached and it would absorb
        // every claim forever. Refuse to start rather than mis-pay.
        validate_claim_targets(&wallet_api, &settings.auto_claim.targets).await?;

        // Dedicated thread pool for the CPU-heavy ticket search, keeping it off
        // Tokio's runtime threads.
        let pool = build_search_pool(settings.mining.max_threads);

        // Stream of winning PoW tickets, one per solved puzzle.
        let mut winning_tickets = TicketGenerator::new::<Tx, _, _>(
            cryptarchia_api.clone(),
            pool,
            settings.mining.max_tickets_per_block,
            settings.slot_window,
        )
        .await?;

        // Processed-block stream, watched to retire pending claims once their
        // reward is observed as settled on chain.
        let mut processed_blocks =
            BroadcastStream::new(cryptarchia_api.subscribe_new_blocks().await?);

        let mut inbound_relay = service_resources_handle.inbound_relay;
        // Persists the claimable/pending tickets so they survive restarts.
        let state_updater = service_resources_handle.state_updater;
        // Mining is off until explicitly started and is not persisted: a
        // restarted node does not resume mining automatically.
        let mut mining = false;

        // Auto-claim arms itself when targets are configured, and disarms once
        // every target has reached its threshold. Like `mining` it is a
        // runtime flag, so a restart re-arms it and the thresholds are
        // re-evaluated against fresh balances.
        let auto_claim = &settings.auto_claim;
        let mut auto_claiming = !auto_claim.targets.is_empty();

        // One stream for either pacing, so the run loop has a single arm and
        // neither kind needs a guard. Slot pacing rides the time service's own
        // slot clock rather than block arrivals, so it keeps ticking through a
        // gap in block production.
        let mut claim_ticks = auto_claim_tick_stream::<TimeBackendType, _>(
            auto_claim.tick,
            &service_resources_handle.overwatch_handle,
        )
        .await?;

        service_resources_handle.status_updater.notify_ready();

        loop {
            tokio::select! {
                Some(message) = inbound_relay.recv() => {
                    match message {
                        PoWServiceMessage::StartMining => {
                            if !mining {
                                info!(target: LOG_TARGET, "PoW mining started");
                            }
                            mining = true;
                        }
                        PoWServiceMessage::StopMining => {
                            if mining {
                                info!(target: LOG_TARGET, "PoW mining stopped");
                            }
                            mining = false;
                        }
                        PoWServiceMessage::StartAutoClaim => {
                            if auto_claim.targets.is_empty() {
                                warn!(target: LOG_TARGET, "PoW auto-claim not started: no claim targets configured");
                            } else {
                                if !auto_claiming {
                                    info!(target: LOG_TARGET, "PoW auto-claim started");
                                }
                                auto_claiming = true;
                            }
                        }
                        PoWServiceMessage::StopAutoClaim => {
                            if auto_claiming {
                                info!(target: LOG_TARGET, "PoW auto-claim stopped");
                            }
                            auto_claiming = false;
                        }
                        PoWServiceMessage::Claim { claim_address, response } => {
                            let result = manual_claim(
                                &cryptarchia_api,
                                &blend_api,
                                &wallet_api,
                                claim_address,
                                &auto_claim.targets,
                                &mut state,
                                settings.slot_window,
                            )
                            .await
                            .inspect_err(|e| {
                                error!(target: LOG_TARGET, "Failed to claim PoW rewards: {e}");
                            });
                            state_updater.update(Some(state.clone()));
                            if response.send(result).is_err() {
                                error!(target: LOG_TARGET, "Claim response receiver was dropped");
                            }
                        }
                        PoWServiceMessage::ClaimableRewardsInfo { response } => {
                            respond_claimable_rewards(&cryptarchia_api, &mut state, &state_updater, response, settings.slot_window).await;
                        }
                    }
                }
                // A puzzle was solved: accumulate the winning ticket to be
                // claimed on demand (only while mining is enabled).
                Some(winning_ticket) = winning_tickets.next(), if mining => {
                    // The new ticket's slot tracks the tip, so use it to drop any
                    // previously stored tickets whose window has since closed.
                    let current_slot = winning_ticket.block_slot;
                    state.ready_to_claim.push(winning_ticket);
                    prune_expired_tickets(&mut state, current_slot, settings.slot_window);
                    info!(
                        target: LOG_TARGET,
                        "Mined a winning ticket 💲; total claimable tickets {}",
                        state.ready_to_claim.len()
                    );
                    state_updater.update(Some(state.clone()));
                }
                // A block was processed: retire any pending claim whose reward
                // note it minted (i.e. the claim has settled on chain).
                Some(processed_block) = processed_blocks.next() => {
                    retire_settled_claims(&cryptarchia_api, &mut state, &state_updater, processed_block).await;
                }
                // Auto-claim tick: drain the ready tickets into the neediest
                // target.
                Some(()) = claim_ticks.next(), if auto_claiming => {
                    auto_claiming = run_auto_claim(
                        &cryptarchia_api,
                        &blend_api,
                        &wallet_api,
                        &auto_claim.targets,
                        &mut state,
                        &state_updater,
                        settings.slot_window,
                    )
                    .await;
                }
            }
        }
    }
}

/// Builds the auto-claim ticker for the configured pacing.
///
/// Both kinds collapse to the same `Stream<Item = ()>` so the run loop needs a
/// single `select!` arm. Slot pacing subscribes to the time service's slot
/// clock rather than counting block arrivals: the two agree while the chain is
/// producing, but only the clock keeps ticking through a lull, which is when a
/// backlog of unclaimed tickets is most likely to be sitting around.
async fn auto_claim_tick_stream<TimeBackendType, RuntimeServiceId>(
    tick: AutoClaimTick,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<Pin<Box<dyn Stream<Item = ()> + Send>>, PoWError>
where
    TimeBackendType: TimeBackend + Send + Sync + 'static,
    TimeBackendType::Settings: Send + Sync,
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<TimeService<TimeBackendType, RuntimeServiceId>>,
{
    match tick {
        AutoClaimTick::Seconds(seconds) => Ok(Box::pin(
            IntervalStream::new(tokio::time::interval(Duration::from_secs(seconds.get())))
                .map(|_| ()),
        )),
        AutoClaimTick::Slots(period) => {
            let time_relay = overwatch_handle
                .relay::<TimeService<TimeBackendType, RuntimeServiceId>>()
                .await
                .map_err(PoWError::TimeRelay)?;
            let (sender, receiver) = oneshot::channel();
            time_relay
                .send(TimeServiceMessage::Subscribe { sender })
                .await
                .map_err(|(relay_error, _)| PoWError::TimeRelay(relay_error))?;
            let slot_ticks = receiver.await?;

            // The stream emits every slot, so thin it down to one item per
            // period. `None` fires on the first slot seen rather than waiting
            // out a full period from an arbitrary starting point.
            //
            // The item is a nested `Option` on purpose: `scan` ends the stream
            // once its closure resolves to `None`, so a skipped slot has to
            // yield `Some(None)` to keep the ticker alive. `filter_map` then
            // drops those inner `None`s, leaving one item per elapsed period.
            Ok(Box::pin(
                slot_ticks
                    .scan(None, move |state, slot_tick| match state {
                        Some(last_claim)
                            if slot_period_elapsed(*last_claim, slot_tick.slot, period) =>
                        {
                            *state = Some(slot_tick.slot);
                            ready(Some(Some(())))
                        }
                        None => {
                            *state = Some(slot_tick.slot);
                            ready(Some(Some(())))
                        }
                        _ => {
                            // non-elapsed periods
                            ready(Some(None))
                        }
                    })
                    .filter_map(ready),
            ))
        }
    }
}

/// Whether `period` slots have passed since the last slot-paced auto-claim.
///
/// The first observed tip counts as elapsed, so a freshly started node claims
/// as soon as it sees a block rather than waiting out a full period.
fn slot_period_elapsed(last_claim_slot: Slot, tip_slot: Slot, period: NonZeroU64) -> bool {
    u64::from(tip_slot).saturating_sub(u64::from(last_claim_slot)) >= period.get()
}

/// Rejects any auto-claim target the wallet does not track.
///
/// The wallet only indexes UTXOs for the keys in its `known_keys` setting, so
/// an unlisted target always reports an empty balance: it would look
/// permanently furthest below its threshold and swallow every claim. Failing
/// here aborts node startup, which is the honest outcome for a
/// misconfiguration that cannot be detected later.
async fn validate_claim_targets<WalletService, RuntimeServiceId>(
    wallet_api: &WalletApi<WalletService, RuntimeServiceId>,
    targets: &[ClaimTarget],
) -> Result<(), PoWError>
where
    WalletService: WalletServiceData,
    RuntimeServiceId: AsServiceId<WalletService> + Debug + Display + Sync,
{
    if targets.is_empty() {
        return Ok(());
    }
    let known: HashSet<ZkPublicKey> = wallet_api
        .get_known_addresses()
        .await?
        .into_iter()
        .collect();
    let unknown: Vec<ZkPublicKey> = targets
        .iter()
        .map(|target| target.public_key)
        .filter(|pk| !known.contains(pk))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(PoWError::UntrackedClaimTargets(unknown))
}

/// Picks the auto-claim target to pay next: among the targets still below their
/// threshold, the one holding the least value.
///
/// Balances are read from the wallet exactly as they stand, with no allowance
/// for claims already published but not yet settled. A tick therefore sees the
/// same balance throughout and pays a single target; the next tick, once those
/// claims have landed, moves on. Returns `None` when every target has reached
/// its threshold.
async fn select_claim_target<WalletService, RuntimeServiceId>(
    wallet_api: &WalletApi<WalletService, RuntimeServiceId>,
    targets: &[ClaimTarget],
) -> Result<Option<ZkPublicKey>, WalletApiError>
where
    WalletService: WalletServiceData,
    RuntimeServiceId: AsServiceId<WalletService> + Debug + Display + Sync,
{
    let mut balances = Vec::with_capacity(targets.len());
    for target in targets {
        // `None` means the wallet tracks the key but it holds nothing yet;
        // untracked keys are rejected at startup by `validate_claim_targets`.
        let balance = wallet_api
            .get_balance(None, target.public_key)
            .await?
            .response
            .map_or(0, |balance| balance.balance);
        balances.push((*target, balance));
    }
    Ok(neediest_target(balances))
}

/// The choice behind [`select_claim_target`], over already-read balances: of
/// the targets still below their threshold, the one holding the least.
///
/// Ties keep the earliest configured target, so the choice is deterministic
/// across ticks that observe the same balances.
fn neediest_target(
    balances: impl IntoIterator<Item = (ClaimTarget, Value)>,
) -> Option<ZkPublicKey> {
    balances
        .into_iter()
        .filter(|(target, balance)| *balance < target.threshold)
        .min_by_key(|(_, balance)| *balance)
        .map(|(target, _)| target.public_key)
}

/// Runs one auto-claim tick, returning whether auto-claim should stay armed.
///
/// The tick picks a single target and keeps publishing claim transactions into
/// it until no ready ticket can be claimed. A batch is capped only by the op
/// budget and the reward pool, so the target may overshoot its threshold — the
/// threshold is where we stop *choosing* it, not a cap on a single payment.
///
/// Returns `false` once every target has reached its threshold, which disarms
/// the ticker until an operator re-arms it with
/// [`PoWServiceMessage::StartAutoClaim`].
async fn run_auto_claim<CryptarchiaService, BlendService, WalletService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    blend_api: &BlendServiceApi<BlendService, RuntimeServiceId>,
    wallet_api: &WalletApi<WalletService, RuntimeServiceId>,
    targets: &[ClaimTarget],
    state: &mut PoWServiceState,
    state_updater: &StateUpdater<Option<PoWServiceState>>,
    slot_window: NonZeroU64,
) -> bool
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    WalletService: WalletServiceData,
    RuntimeServiceId: AsServiceId<WalletService> + Debug + Display + Sync,
{
    // Nothing mined since the last tick: skip the wallet round-trip entirely.
    if state.ready_to_claim.is_empty() {
        return true;
    }

    let claim_address = match select_claim_target(wallet_api, targets).await {
        Ok(Some(claim_address)) => claim_address,
        Ok(None) => {
            info!(
                target: LOG_TARGET,
                "Every PoW auto-claim target reached its threshold; stopping auto-claim"
            );
            return false;
        }
        Err(e) => {
            error!(target: LOG_TARGET, "Failed to pick a PoW auto-claim target: {e}");
            return true;
        }
    };

    drain_ready_rewards(
        cryptarchia_api,
        blend_api,
        claim_address,
        state,
        state_updater,
        slot_window,
    )
    .await;
    true
}

/// Publishes claim transactions to `claim_address` until no ready ticket can be
/// claimed, or the reward pool runs dry.
///
/// One transaction only carries so many claims (the op budget and the reward
/// pool both cap it), so emptying a backlog takes several rounds.
///
/// The pool is tracked here rather than re-read per round. A published claim
/// does not reach the tip until it settles, so the tip keeps reporting the same
/// unspent pool and would re-authorise funds this loop has already committed —
/// emitting a burst of transactions the chain then rejects. Drawing down a
/// local balance stops the loop when the funds are spoken for; the next tick
/// starts again from whatever the chain actually reports.
async fn drain_ready_rewards<CryptarchiaService, BlendService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    blend_api: &BlendServiceApi<BlendService, RuntimeServiceId>,
    claim_address: ZkPublicKey,
    state: &mut PoWServiceState,
    state_updater: &StateUpdater<Option<PoWServiceState>>,
    slot_window: NonZeroU64,
) where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    RuntimeServiceId: Sync,
{
    // The running balance every claim in this drain spends against, opened at
    // whatever the chain currently reports.
    let mut available_pool = match current_reward_pool(cryptarchia_api).await {
        Ok(pool) => pool,
        Err(e) => {
            error!(target: LOG_TARGET, "Failed to read the PoW reward pool: {e}");
            return;
        }
    };
    loop {
        match claim_ready_rewards(
            cryptarchia_api,
            blend_api,
            claim_address,
            state,
            slot_window,
            available_pool,
        )
        .await
        {
            // Nothing left that can be claimed right now: the ready set is
            // empty, or what remains is anchored to non-canonical blocks.
            Ok(None) => break,
            // A published claim always consumes at least one ready ticket, so
            // the loop is guaranteed to terminate.
            Ok(Some(claim)) => {
                available_pool = claim.remaining_pool;
                state_updater.update(Some(state.clone()));
            }
            // `RewardPoolExhausted` is the expected end of a drain once the
            // pool is spent, so it is not worth an error. Anything else stops
            // the loop too, leaving the tickets ready for the next tick.
            Err(PoWError::RewardPoolExhausted) => {
                info!(
                    target: LOG_TARGET,
                    "PoW reward pool spent; {} ticket(s) still ready",
                    state.ready_to_claim.len()
                );
                break;
            }
            Err(e) => {
                error!(target: LOG_TARGET, "PoW auto-claim failed: {e}");
                break;
            }
        }
    }
    state_updater.update(Some(state.clone()));
}

/// Serves a [`PoWServiceMessage::Claim`]: one claim transaction paid to
/// `claim_address`, or to the target auto-claim would pick when the caller did
/// not name a key.
async fn manual_claim<CryptarchiaService, BlendService, WalletService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    blend_api: &BlendServiceApi<BlendService, RuntimeServiceId>,
    wallet_api: &WalletApi<WalletService, RuntimeServiceId>,
    claim_address: Option<ZkPublicKey>,
    targets: &[ClaimTarget],
    state: &mut PoWServiceState,
    slot_window: NonZeroU64,
) -> Result<Option<TxHash>, PoWError>
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    WalletService: WalletServiceData,
    RuntimeServiceId: AsServiceId<WalletService> + Debug + Display + Sync,
{
    if state.ready_to_claim.is_empty() {
        info!(target: LOG_TARGET, "No PoW rewards to claim");
        return Ok(None);
    }
    let claim_address = match claim_address {
        Some(claim_address) => claim_address,
        None => select_claim_target(wallet_api, targets)
            .await?
            .ok_or(PoWError::NoClaimTarget)?,
    };
    // A one-off claim has no run to accumulate over, so its balance is simply
    // the pool the chain reports right now, and the leftover is discarded.
    let available_pool = current_reward_pool(cryptarchia_api).await?;
    Ok(claim_ready_rewards(
        cryptarchia_api,
        blend_api,
        claim_address,
        state,
        slot_window,
        available_pool,
    )
    .await?
    .map(|claim| claim.tx_hash))
}

/// The reward pool at the current tip: the opening balance for a run of claims.
async fn current_reward_pool<CryptarchiaService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
) -> Result<Value, PoWError>
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    RuntimeServiceId: Sync,
{
    let tip = cryptarchia_api.info().await?.cryptarchia_info.tip;
    let ledger_state = cryptarchia_api
        .get_ledger_state(tip)
        .await?
        .ok_or(PoWError::LedgerStateUnavailable(tip))?;
    Ok(ledger_state.mantle_ledger().pow.reward_pool())
}

/// A published reward-claim transaction.
struct PublishedClaim {
    tx_hash: TxHash,
    /// The reward pool left over once this claim is paid for, to be carried
    /// into the next claim of the same run.
    remaining_pool: Value,
}

/// Builds and publishes a reward-claim transaction for every ticket currently
/// ready to claim, moving the claimed tickets to the pending set on success.
///
/// `available_pool` is the reward pool this batch may spend; the balance left
/// over comes back in [`PublishedClaim::remaining_pool`]. A run of claims
/// therefore threads one balance from call to call rather than re-reading the
/// chain: a published claim does not reach the tip until it settles, so the tip
/// would keep re-authorising funds already committed. Open the run with
/// [`current_reward_pool`].
///
/// On any failure the ready set is left untouched so the tickets can be
/// retried.
async fn claim_ready_rewards<CryptarchiaService, BlendService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    blend_api: &BlendServiceApi<BlendService, RuntimeServiceId>,
    claim_address: ZkPublicKey,
    state: &mut PoWServiceState,
    slot_window: NonZeroU64,
    available_pool: Value,
) -> Result<Option<PublishedClaim>, PoWError>
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    RuntimeServiceId: Sync,
{
    // Size the batch against the current tip — the state the tx applies
    // against.
    let info = cryptarchia_api.info().await?.cryptarchia_info;

    // Drop any tickets whose window has closed before building the batch, so an
    // expired ticket never poisons the tx.
    prune_expired_tickets(state, info.slot, slot_window);
    if state.ready_to_claim.is_empty() {
        return Ok(None);
    }

    let ledger_state = cryptarchia_api
        .get_ledger_state(info.tip)
        .await?
        .ok_or(PoWError::LedgerStateUnavailable(info.tip))?;

    // Build the tx only from tickets whose anchor block is still on the
    // canonical chain: on chain `accept_claim` requires the block to be in this
    // branch's known-block set (the map below), so a non-canonical claim —
    // e.g. one orphaned by a reorg — would be rejected and poison the whole tx.
    // Such tickets are *kept* in `ready_to_claim` (a later reorg may restore
    // their block); only the window check in `prune_expired_tickets` discards
    // tickets for good.
    let known_blocks = ledger_state.mantle_ledger().pow.block_slots();
    let tickets: Vec<(UnsecuredZkKey, ClaimPowRewardOp)> = state
        .ready_to_claim
        .iter()
        .filter(|ticket| known_blocks.contains_key(&ticket.claim.block_hash))
        .map(|ticket| (ticket.secret_key.clone(), ticket.claim.clone()))
        .collect();
    if tickets.is_empty() {
        return Ok(None);
    }

    // Only mutate the ready set once the tx is built and published, so a
    // failure leaves every ticket in place for a later retry. The builder
    // caps the batch (op limit / reward pool) and claims a prefix of
    // `tickets` (the canonical ones, in ready order); move exactly those to
    // the pending set.
    let (signed_tx, claimed_count) =
        build_reward_claim_tx(claim_address, &ledger_state, available_pool, &tickets).await?;
    // Capture the tx id before publishing consumes the signed tx, so it can be
    // reported back to the caller.
    let tx_hash = signed_tx.hash();
    publish_reward_claim(blend_api, signed_tx).await?;

    let mut remaining = Vec::with_capacity(state.ready_to_claim.len());
    let mut claimed = Vec::with_capacity(claimed_count);
    for ticket in state.ready_to_claim.drain(..) {
        // Take the first `claimed_count` canonical tickets — the exact prefix
        // the builder consumed — and keep everything else ready.
        if claimed.len() < claimed_count && known_blocks.contains_key(&ticket.claim.block_hash) {
            claimed.push(ticket);
        } else {
            remaining.push(ticket);
        }
    }
    state.ready_to_claim = remaining;

    info!(
        target: LOG_TARGET,
        "Claimed {} PoW reward(s); {} still ready",
        claimed.len(),
        state.ready_to_claim.len()
    );
    state.pending_to_claim.extend(claimed);
    // Debit what this claim committed. The builder sized the batch against
    // `available_pool`, so this cannot underflow; saturating only guards a
    // future change to that invariant.
    let remaining_pool = available_pool.saturating_sub(
        (claimed_count as Value).saturating_mul(ledger_state.mantle_ledger().pow.epoch_reward()),
    );
    Ok(Some(PublishedClaim {
        tx_hash,
        remaining_pool,
    }))
}

/// Whether a ticket anchored to a block at `block_slot` is still within its
/// reward window at `current_slot`: claimable while
/// `current_slot - block_slot <= slot_window`.
fn is_within_reward_window(block_slot: Slot, current_slot: Slot, slot_window: NonZeroU64) -> bool {
    u64::from(block_slot)
        .checked_add(slot_window.get())
        .is_some_and(|last_claimable_slot| last_claimable_slot >= u64::from(current_slot))
}

/// Drops tickets whose reward window has closed at `current_slot`.
///
/// Once a ticket falls out of the window it can never be claimed, so keeping it
/// only bloats the state and would poison a claim tx built from the batch. Both
/// the ready and pending sets are pruned in place.
fn prune_expired_tickets(state: &mut PoWServiceState, current_slot: Slot, slot_window: NonZeroU64) {
    let before = state.ready_to_claim.len() + state.pending_to_claim.len();
    state
        .ready_to_claim
        .retain(|ticket| is_within_reward_window(ticket.block_slot, current_slot, slot_window));
    state
        .pending_to_claim
        .retain(|ticket| is_within_reward_window(ticket.block_slot, current_slot, slot_window));
    let pruned = before - (state.ready_to_claim.len() + state.pending_to_claim.len());
    if pruned > 0 {
        info!(target: LOG_TARGET, "Pruned {pruned} expired PoW ticket(s)");
    }
}

/// Answers a [`PoWServiceMessage::ClaimableRewardsInfo`] query: prunes expired
/// tickets against the current slot, persists the pruned state, then reports
/// the still-claimable ones.
async fn respond_claimable_rewards<CryptarchiaService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    state: &mut PoWServiceState,
    state_updater: &StateUpdater<Option<PoWServiceState>>,
    response: oneshot::Sender<ClaimableRewardsInfo>,
    slot_window: NonZeroU64,
) where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    RuntimeServiceId: Sync,
{
    let current_slot = match cryptarchia_api.info().await {
        Ok(info) => info.cryptarchia_info.slot,
        Err(e) => {
            error!(target: LOG_TARGET, "Failed to query chain info for claimable rewards: {e}");
            return;
        }
    };
    // Drop expired tickets first, so the report covers only what is still
    // claimable.
    prune_expired_tickets(state, current_slot, slot_window);
    state_updater.update(Some(state.clone()));
    let claimable = claimable_rewards_info(&state.ready_to_claim, current_slot, slot_window);
    if response.send(claimable).is_err() {
        error!(target: LOG_TARGET, "ClaimableRewardsInfo response receiver was dropped");
    }
}

/// Handles one processed-block event: retires any pending claim it settled and
/// persists the state when it changes.
///
/// A missed broadcast event is logged and ignored; a fresh subscription always
/// re-emits the current tip, so a later block covers any settlement in the gap.
async fn retire_settled_claims<CryptarchiaService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    state: &mut PoWServiceState,
    state_updater: &StateUpdater<Option<PoWServiceState>>,
    processed_block: Result<ProcessedBlockEvent, BroadcastStreamRecvError>,
) where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    RuntimeServiceId: Sync,
{
    let block_id = match processed_block {
        Ok(block) => block.block_id,
        Err(e) => {
            warn!(target: LOG_TARGET, "Missed a processed-block event: {e}");
            return;
        }
    };
    match prune_settled_pending(cryptarchia_api, state, block_id).await {
        Ok(0) => {}
        Ok(settled) => {
            info!(
                target: LOG_TARGET,
                "Retired {settled} settled PoW claim(s); {} still pending",
                state.pending_to_claim.len()
            );
            state_updater.update(Some(state.clone()));
        }
        Err(e) => {
            error!(target: LOG_TARGET, "Failed to check settled PoW claims: {e}");
        }
    }
}

/// Retires pending tickets whose reward has settled on chain, detected by a
/// [`TxEventPayload::PoWRewardClaimed`] event in the just-processed `block_id`.
///
/// The event carries the spent solution's [`PowNullifier`], which is exactly a
/// claim's puzzle ticket, so a pending ticket is matched by re-deriving that
/// nullifier from its claim. Returns the number of tickets retired.
async fn prune_settled_pending<CryptarchiaService, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    state: &mut PoWServiceState,
    block_id: HeaderId,
) -> Result<usize, PoWError>
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    RuntimeServiceId: Sync,
{
    if state.pending_to_claim.is_empty() {
        return Ok(0);
    }
    let Some(events) = cryptarchia_api.get_block_events(block_id).await? else {
        return Ok(0);
    };
    let settled: HashSet<PowNullifier> = events
        .iter()
        .filter_map(|event| match event {
            Event::Tx(TxEvent {
                payload: TxEventPayload::PoWRewardClaimed { pow_nullifier, .. },
                ..
            }) => Some(*pow_nullifier),
            _ => None,
        })
        .collect();
    if settled.is_empty() {
        return Ok(0);
    }

    let before = state.pending_to_claim.len();
    state
        .pending_to_claim
        .retain(|ticket| !settled.contains(&ticket.claim.get_puzzle_ticket()));
    Ok(before - state.pending_to_claim.len())
}

/// Summarizes how long each ready ticket remains within the reward window.
///
/// Callers prune expired tickets first (see [`prune_expired_tickets`]), so
/// every ticket here is assumed to still be within the window: a ticket
/// anchored to a block at `block_slot` has `block_slot + slot_window -
/// current_slot` slots of remaining lifetime.
fn claimable_rewards_info(
    ready_to_claim: &[WinningTicket],
    current_slot: Slot,
    slot_window: NonZeroU64,
) -> ClaimableRewardsInfo {
    let current = u64::from(current_slot);
    let slots_until_expiry: Vec<Slot> = ready_to_claim
        .iter()
        .map(|ticket| Slot::new(u64::from(ticket.block_slot) + slot_window.get() - current))
        .collect();
    ClaimableRewardsInfo {
        claimable_tickets: slots_until_expiry.len(),
        slots_until_expiry,
    }
}

/// Largest claim count whose ops — the claims plus the `ceil(c / 32)` transfers
/// that spend their reward notes — fit within [`MAX_OPS_PER_TX`].
const fn max_claims_by_ops() -> usize {
    let mut claims: usize = 0;
    while (claims + 1) + (claims + 1).div_ceil(MAX_TRANSFER_INPUTS) <= MAX_OPS_PER_TX {
        claims += 1;
    }
    claims
}

/// Size, in bytes, of the claim transaction a batch of `claims` tickets
/// produces once signed, measured through the codec [`publish_reward_claim`]
/// encodes with on a transaction of exactly that shape.
///
/// The probe is assembled from zeroed claims and a zeroed signature, and
/// shares [`transfer_ops`], [`push_reward_claim_ops`] and
/// [`claim_ops_proofs`] with the real builder so its shape cannot drift from
/// one. Values do not change the encoded length — the codec uses fixed-width
/// integers, and a signature is a fixed-size byte triple — so the probe
/// measures the real batch, and nothing here is signed: it costs microseconds
/// rather than a proof per transfer group.
///
/// [`claim_tx_size_matches_a_signed_transaction`] pins that equivalence.
fn claim_tx_size(claims: usize) -> Result<u64, PoWError> {
    let claim = ClaimPowRewardOp {
        epoch_nonce: *ZkPublicKey::zero().as_fr(),
        block_hash: [0u8; 32],
        public_key: ZkPublicKey::zero(),
    };
    let signature = ZkSignature::new(ZkSignProof::from_bytes(&[0u8; COMPRESSED_PROOF_SIZE]));
    let groups = claims.div_ceil(MAX_TRANSFER_INPUTS);

    let probe_claims = vec![claim.clone(); claims];
    let note_ids = vec![Utxo::new(claim.op_id(), 0, Note::new(0, claim.public_key)).id(); claims];
    let transfers = transfer_ops(&note_ids, ZkPublicKey::zero(), &vec![0; groups])?;

    let ops = push_reward_claim_ops(MantleTxBuilder::new(), &probe_claims, transfers)?.build()?;
    let ops_proofs = claim_ops_proofs(claims, std::iter::repeat_n(signature, groups))?;

    Ok(SerializeOp::bytes_size(
        &SignedOps::<_, StandardMode>::from_parts(ops, ops_proofs)?,
    )?)
}

/// Largest claim count whose transaction still fits the body of a Blend
/// payload, the transport every claim is published over.
///
/// This binds well before [`max_claims_by_ops`] does: 255 ops' worth of claims
/// serializes to nearly twice what a payload can carry, and a transaction over
/// the limit is rejected by [`publish_reward_claim`] *after* its (CPU-heavy)
/// signatures have been produced. Capping the batch here keeps that failure
/// from ever being reached.
///
/// Resolved once, by shrinking a probe batch from the op cap until it fits, so
/// the limit follows the encoding rather than restating it.
static MAX_CLAIMS_BY_PAYLOAD_SIZE: LazyLock<usize> = LazyLock::new(|| {
    (1..=max_claims_by_ops())
        .rev()
        .find(|&claims| {
            claim_tx_size(claims).is_ok_and(|size| size <= MAX_PAYLOAD_BODY_SIZE as u64)
        })
        .expect("a single-claim tx fits a Blend payload")
});

/// Builds and signs a single self-funding reward-claim transaction from a batch
/// of winning tickets.
///
/// For every group of up to 32 claims (32 being the multi-signature key limit)
/// a `Transfer` op spends the reward notes those claims mint, so the tx
/// interleaves as `[claims 1..32], transfer, [claims 33..64], transfer, ...`.
/// Each group's claims precede its transfer, so the transfer can reference the
/// freshly minted UTXOs, reconstructed here from the same data the ledger uses,
/// and is signed by those notes' owning keys.
///
/// Only as many claims are taken as `reward_pool` can fund, the per-tx op
/// limit allows, and a Blend payload can carry. Returns the signed tx together
/// with the number of tickets (a prefix of `tickets`) it actually claims.
///
/// `reward_pool` is passed in rather than read from `ledger_state` so a caller
/// publishing several transactions back to back can draw down its own running
/// balance; see [`drain_ready_rewards`].
async fn build_reward_claim_tx(
    claim_address: ZkPublicKey,
    ledger_state: &LedgerState,
    reward_pool: Value,
    tickets: &[(UnsecuredZkKey, ClaimPowRewardOp)],
) -> Result<(SignedOps<Unverified, StandardMode>, usize), PoWError> {
    // The reward value and gas prices are read at `ledger_state`; they must
    // match the state the tx applies against, or the reconstructed UTXOs / fee
    // will be off.
    build_reward_claim_tx_inner(
        claim_address,
        ledger_state.mantle_ledger().pow.epoch_reward(),
        reward_pool,
        ledger_state.get_gas_prices(),
        tickets,
    )
    .await
}

/// Inner builder over plain reward/gas values, so it can be exercised without a
/// full [`LedgerState`]. See [`build_reward_claim_tx`].
async fn build_reward_claim_tx_inner(
    claim_address: ZkPublicKey,
    reward_value: Value,
    reward_pool: Value,
    gas_prices: GasPrices,
    tickets: &[(UnsecuredZkKey, ClaimPowRewardOp)],
) -> Result<(SignedOps<Unverified, StandardMode>, usize), PoWError> {
    if reward_value == 0 {
        return Err(PoWError::RewardsDisabled);
    }
    let context = OpsContext {
        gas_context: OpsGasContext::new(HashMap::new(), HashMap::new(), gas_prices),
        leader_reward_amount: 0,
    };

    // Take as many claims as the pool can fund, the op budget allows, and a
    // Blend payload can carry.
    let claim_count = tickets
        .len()
        .min((reward_pool / reward_value) as usize)
        .min(max_claims_by_ops())
        .min(*MAX_CLAIMS_BY_PAYLOAD_SIZE);
    if claim_count == 0 {
        return Err(PoWError::RewardPoolExhausted);
    }
    let tickets = &tickets[..claim_count];
    let claims: Vec<ClaimPowRewardOp> = tickets.iter().map(|(_, claim)| claim.clone()).collect();

    // Reconstruct the id of the UTXO each claim mints (op_id, output 0, reward
    // note), so the transfers can spend them.
    let note_ids: Vec<NoteId> = claims
        .iter()
        .map(|claim| Utxo::new(claim.op_id(), 0, Note::new(reward_value, claim.public_key)).id())
        .collect();

    // Size the change against the final tx shape, then spread the fee across
    // the transfer outputs.
    let fee = estimate_reward_claim_fee(&claims, &note_ids, claim_address, &context)?;
    let change_outputs = change_outputs(&note_ids, reward_value, fee)?;
    let transfers = transfer_ops(&note_ids, claim_address, &change_outputs)?;

    let mantle_tx = push_reward_claim_ops(MantleTxBuilder::new(), &claims, transfers)?.build()?;

    // Sign each transfer with the keys owning its input notes (a
    // multi-signature over the whole tx hash). Signing is a ZK proof
    // (CPU-heavy), so run it off the async runtime.
    let tx_fr = mantle_tx.hash().to_fr();
    let sk_groups: Vec<Vec<UnsecuredZkKey>> = tickets
        .chunks(MAX_TRANSFER_INPUTS)
        .map(|group| group.iter().map(|(sk, _)| sk.clone()).collect())
        .collect();
    let zk_sigs = tokio::task::spawn_blocking(move || {
        sk_groups
            .iter()
            .map(|sks| UnsecuredZkKey::multi_sign(sks, &tx_fr))
            .collect::<Result<Vec<_>, _>>()
    })
    .await??;

    let ops_proofs = claim_ops_proofs(claim_count, zk_sigs)?;
    let tx = SignedOps::from_parts(mantle_tx, ops_proofs)?;

    Ok((tx, claim_count))
}

/// Builds the `Transfer` ops spending `note_ids`, grouped into batches of up to
/// [`MAX_TRANSFER_INPUTS`] inputs, each returning `change_outputs[group]` to
/// `claim_address`.
fn transfer_ops(
    note_ids: &[NoteId],
    claim_address: ZkPublicKey,
    change_outputs: &[Value],
) -> Result<Vec<TransferOp>, PoWError> {
    note_ids
        .chunks(MAX_TRANSFER_INPUTS)
        .zip(change_outputs)
        .map(|(group, &change)| {
            Ok(TransferOp::new(
                Inputs::try_new(group.to_vec())?,
                Outputs::new([Note::new(change, claim_address)]),
            ))
        })
        .collect()
}

/// Pushes the interleaved reward-claim ops onto `builder`: for every group of
/// up to [`MAX_TRANSFER_INPUTS`] claims, the claim ops followed by that group's
/// transfer. This is where the leaf ops are wrapped into their [`Op`] variants.
fn push_reward_claim_ops(
    mut builder: MantleTxBuilder,
    claims: &[ClaimPowRewardOp],
    transfers: Vec<TransferOp>,
) -> Result<MantleTxBuilder, PoWError> {
    for (claim_group, transfer) in claims.chunks(MAX_TRANSFER_INPUTS).zip(transfers) {
        builder = builder
            .extend_ops(claim_group.iter().cloned().map(Op::ClaimPowReward))?
            .push_op(Op::Transfer(transfer))?;
    }
    Ok(builder)
}

/// The proof list a claim transaction carries, in op order: a `None` proof per
/// claim in a group, then that group's transfer signature.
///
/// Shared with [`claim_tx_size`], so a probe transaction is proved-for exactly
/// as the real one is.
fn claim_ops_proofs(
    claims: usize,
    zk_sigs: impl IntoIterator<Item = ZkSignature>,
) -> Result<OpProofs, PoWError> {
    let mut ops_proofs = OpProofs::empty();
    let mut remaining = claims;
    for zk_sig in zk_sigs {
        let group = remaining.min(MAX_TRANSFER_INPUTS);
        remaining -= group;
        for _ in 0..group {
            ops_proofs.try_push(OpProof::None(NoOpProof))?;
        }
        ops_proofs.try_push(OpProof::ZkSig(zk_sig))?;
    }
    Ok(ops_proofs)
}

/// The change value each transfer group returns: the reward its notes carry,
/// less its share of the fee.
///
/// The fee is charged group by group, each absorbing as much as its reward
/// allows before the remainder spills into the next. This lets the whole batch
/// cover a fee larger than any single group's reward; only a fee exceeding the
/// *total* reward fails with [`PoWError::RewardBelowFee`].
fn change_outputs(
    note_ids: &[NoteId],
    reward_value: Value,
    fee: Value,
) -> Result<Vec<Value>, PoWError> {
    let mut fee_remaining = fee;
    let mut outputs = Vec::new();
    for group in note_ids.chunks(MAX_TRANSFER_INPUTS) {
        let group_reward = (group.len() as Value)
            .checked_mul(reward_value)
            .ok_or(PoWError::RewardOverflow)?;
        let fee_charged = fee_remaining.min(group_reward);
        fee_remaining -= fee_charged;
        outputs.push(group_reward - fee_charged);
    }
    if fee_remaining != 0 {
        return Err(PoWError::RewardBelowFee);
    }
    Ok(outputs)
}

/// Publishes a reward-claim transaction over the blend network.
///
/// The tx is encoded the same way the mempool gossips transactions, so
/// whichever node exits the blend network decodes what it expects.
async fn publish_reward_claim<BlendService, RuntimeServiceId>(
    blend_api: &BlendServiceApi<BlendService, RuntimeServiceId>,
    signed_tx: SignedOps<Unverified, StandardMode>,
) -> Result<(), PoWError>
where
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    RuntimeServiceId: Sync,
{
    let payload = BlendPayload::transaction(signed_tx.to_bytes()?.to_vec())?;
    blend_api.publish(payload).await?;
    Ok(())
}

/// Estimates the gas fee of a self-funding reward-claim transaction.
///
/// The fee is the minimum gas cost of the whole `[claims.., transfers..]`
/// shape. The change output values do not affect gas, so it is measured against
/// zero-value outputs.
fn estimate_reward_claim_fee(
    claims: &[ClaimPowRewardOp],
    note_ids: &[NoteId],
    claim_address: ZkPublicKey,
    context: &OpsContext,
) -> Result<Value, PoWError> {
    // Change values don't affect gas, so probe with zero-value outputs.
    let num_groups = note_ids.len().div_ceil(MAX_TRANSFER_INPUTS);
    let transfers = transfer_ops(note_ids, claim_address, &vec![0; num_groups])?;
    let fee = push_reward_claim_ops(MantleTxBuilder::new(), claims, transfers)?
        .minimum_gas_cost::<MainnetGasProfile>(context)?
        .into_inner();
    Ok(fee)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, num::NonZeroU64};

    use lb_chain_service::Slot;
    use lb_core::{
        codec::SerializeOp as _,
        header::HeaderId,
        mantle::{
            Note, NoteId, OpProofRef, OpRef, SignedOps, Utxo,
            ledger::verification_mode::StandardMode,
            ops::{OpId as _, pow::ClaimPowRewardOp},
            traits::MantleTx as _,
            transactions::{
                GasPrices, MAX_OPS_PER_TX, MantleTxBuilder,
                states::Unverified,
                tx_list::ops::{OpsContext, OpsGasContext},
            },
        },
    };
    use lb_key_management_system_keys::keys::{UnsecuredZkKey, ZkPublicKey};

    use super::{
        AutoClaimSettings, AutoClaimTick, ClaimTarget, MAX_CLAIMS_BY_PAYLOAD_SIZE,
        MAX_PAYLOAD_BODY_SIZE, MAX_TRANSFER_INPUTS, PoWError, PoWServiceState,
        build_reward_claim_tx_inner, change_outputs, claim_tx_size, claimable_rewards_info,
        estimate_reward_claim_fee, max_claims_by_ops, neediest_target, prune_expired_tickets,
        push_reward_claim_ops, slot_period_elapsed, transfer_ops,
    };
    use crate::tickets::WinningTicket;

    const REWARD: u64 = 1_000_000;
    const POOL: u64 = 1_000_000_000;

    const SLOT_WINDOW: NonZeroU64 = NonZeroU64::new(100).expect("100 is not 0");
    /// A distinct dummy note id, derived like the ones the builder
    /// reconstructs.
    fn note_id(seed: u8) -> NoteId {
        Utxo::new([seed; 32], 0, Note::new(1, ZkPublicKey::zero())).id()
    }

    /// A ticket whose claim is owned by the ticket's own key.
    fn ticket() -> (UnsecuredZkKey, ClaimPowRewardOp) {
        let secret_key = UnsecuredZkKey::from_rng(&mut rand::thread_rng());
        let claim = ClaimPowRewardOp {
            epoch_nonce: *ZkPublicKey::zero().as_fr(),
            block_hash: [0u8; 32],
            public_key: secret_key.to_public_key(),
        };
        (secret_key, claim)
    }

    fn context() -> OpsContext {
        OpsContext {
            gas_context: OpsGasContext::new(
                HashMap::default(),
                HashMap::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        }
    }

    /// Asserts a built tx respects every limit it has to clear: the op budget,
    /// the per-transfer signing-key limit, a correctly-typed proof per op (via
    /// the ledger's stateless `preverify`), and the Blend payload body it is
    /// published in.
    fn assert_within_tx_limits(tx: &SignedOps<Unverified, StandardMode>) {
        let size = tx.to_bytes().expect("built tx should serialize").len();
        assert!(
            size <= MAX_PAYLOAD_BODY_SIZE,
            "tx of {size} bytes exceeds the {MAX_PAYLOAD_BODY_SIZE} a Blend payload carries"
        );
        let op_refs = tx.op_refs();
        assert!(
            op_refs.len() <= MAX_OPS_PER_TX,
            "op count exceeds the tx limit"
        );
        for op_ref in op_refs {
            if let OpRef::Transfer(transfer) = op_ref {
                assert!(
                    (&transfer.inputs).into_iter().count() <= MAX_TRANSFER_INPUTS,
                    "transfer inputs exceed the signing-key limit"
                );
            }
        }
        tx.clone()
            .preverify()
            .expect("built tx should pass stateless structural verification");
    }

    /// A distinct dummy claim target.
    fn target(seed: u8, threshold: u64) -> ClaimTarget {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        ClaimTarget {
            public_key: ZkPublicKey::new(lb_groth16::fr_from_bytes(&bytes).unwrap()),
            threshold,
        }
    }

    #[test]
    fn neediest_target_picks_the_least_funded_below_its_threshold() {
        let rich = target(1, 1_000);
        let poor = target(2, 1_000);
        let middling = target(3, 1_000);
        let picked = neediest_target([(rich, 900), (poor, 100), (middling, 500)]);
        assert_eq!(picked, Some(poor.public_key));
    }

    #[test]
    fn neediest_target_ignores_satisfied_targets_however_poor() {
        // The poorest key has already met its (much lower) threshold, so the
        // still-hungry one is paid even though it holds more.
        let satisfied = target(1, 100);
        let hungry = target(2, 10_000);
        let picked = neediest_target([(satisfied, 100), (hungry, 500)]);
        assert_eq!(picked, Some(hungry.public_key));
    }

    #[test]
    fn neediest_target_breaks_ties_on_configuration_order() {
        let first = target(1, 1_000);
        let second = target(2, 1_000);
        let picked = neediest_target([(first, 400), (second, 400)]);
        assert_eq!(picked, Some(first.public_key));
    }

    #[test]
    fn neediest_target_is_none_once_every_target_is_satisfied() {
        // What disarms auto-claim: a target exactly at its threshold counts as
        // satisfied.
        let exact = target(1, 1_000);
        let over = target(2, 1_000);
        assert_eq!(neediest_target([(exact, 1_000), (over, 5_000)]), None);
        assert_eq!(neediest_target([]), None);
    }

    #[test]
    fn neediest_target_reads_stale_balances_at_face_value() {
        // Balances are read as they stand, with no allowance for claims already
        // published but not yet settled: a target that a previous tick just
        // paid still looks needy and is picked again.
        let just_paid = target(1, 10_000);
        let other = target(2, 10_000);
        assert_eq!(
            neediest_target([(just_paid, 0), (other, 1)]),
            Some(just_paid.public_key)
        );
    }

    #[test]
    fn slot_period_elapsed_waits_out_a_whole_period() {
        let period = NonZeroU64::new(10).unwrap();
        let last = Slot::new(100);
        assert!(!slot_period_elapsed(last, Slot::new(109), period));
        assert!(slot_period_elapsed(last, Slot::new(110), period));
        assert!(slot_period_elapsed(last, Slot::new(200), period));
    }

    #[test]
    fn slot_period_elapsed_does_not_fire_on_a_reorg_to_an_older_slot() {
        // A slot below the last claim saturates to zero rather than wrapping
        // into a huge elapsed count.
        let period = NonZeroU64::new(10).unwrap();
        assert!(!slot_period_elapsed(Slot::new(100), Slot::new(90), period));
    }

    #[test]
    fn auto_claim_settings_default_to_a_five_minute_tick_and_no_targets() {
        let settings = AutoClaimSettings::default();
        assert_eq!(
            settings.tick,
            AutoClaimTick::Seconds(NonZeroU64::new(300).unwrap())
        );
        assert!(settings.targets.is_empty());
    }

    #[test]
    fn auto_claim_settings_deserialize_from_a_partial_configuration() {
        // An omitted `tick` keeps the default, and both tick kinds parse.
        let only_targets: AutoClaimSettings = serde_json::from_str(
            r#"{"targets": [{"public_key": "0100000000000000000000000000000000000000000000000000000000000000", "threshold": 42}]}"#,
        )
        .unwrap();
        assert_eq!(
            only_targets.tick,
            AutoClaimTick::Seconds(NonZeroU64::new(300).unwrap())
        );
        assert_eq!(only_targets.targets, vec![target(1, 42)]);

        let slot_paced: AutoClaimSettings =
            serde_json::from_str(r#"{"tick": {"unit": "slots", "value": 20}}"#).unwrap();
        assert_eq!(
            slot_paced.tick,
            AutoClaimTick::Slots(NonZeroU64::new(20).unwrap())
        );
        assert!(slot_paced.targets.is_empty());
    }

    #[test]
    fn max_claims_by_ops_saturates_the_op_budget() {
        let claims = max_claims_by_ops();
        assert!(claims + claims.div_ceil(MAX_TRANSFER_INPUTS) <= MAX_OPS_PER_TX);
        assert!((claims + 1) + (claims + 1).div_ceil(MAX_TRANSFER_INPUTS) > MAX_OPS_PER_TX);
    }

    #[test]
    fn change_outputs_returns_reward_minus_fee_for_a_single_group() {
        let notes: Vec<NoteId> = (0u8..5).map(note_id).collect();
        let outputs = change_outputs(&notes, 100, 30).unwrap();
        assert_eq!(outputs, vec![5 * 100 - 30]);
    }

    #[test]
    fn change_outputs_charges_earlier_groups_first() {
        // 40 notes -> two groups of 32 and 8. The first group's reward covers
        // the whole fee, so it absorbs it all.
        let notes: Vec<NoteId> = (0u8..40).map(note_id).collect();
        let outputs = change_outputs(&notes, 100, 30).unwrap();
        assert_eq!(outputs, vec![32 * 100 - 30, 8 * 100]);
    }

    #[test]
    fn change_outputs_spills_the_fee_into_later_groups() {
        // 40 notes -> groups of 32 and 8. Fee 3250 exceeds the first group's
        // 32*100=3200 reward, so the 50 remainder spills into the second group.
        let notes: Vec<NoteId> = (0u8..40).map(note_id).collect();
        let outputs = change_outputs(&notes, 100, 3250).unwrap();
        assert_eq!(outputs, vec![0, 8 * 100 - 50]);
    }

    #[test]
    fn change_outputs_errors_only_when_fee_exceeds_total_reward() {
        // 40 notes -> total reward 4000. A fee of 4000 is fully covered
        // (all outputs zero); one satoshi more cannot be.
        let notes: Vec<NoteId> = (0u8..40).map(note_id).collect();
        assert_eq!(change_outputs(&notes, 100, 4000).unwrap(), vec![0, 0]);
        assert!(matches!(
            change_outputs(&notes, 100, 4001),
            Err(PoWError::RewardBelowFee)
        ));
    }

    #[test]
    fn change_outputs_errors_when_reward_below_fee() {
        let notes = vec![note_id(0)];
        assert!(matches!(
            change_outputs(&notes, 10, 20),
            Err(PoWError::RewardBelowFee)
        ));
    }

    #[test]
    fn change_outputs_errors_on_overflow() {
        let notes: Vec<NoteId> = (0u8..2).map(note_id).collect();
        assert!(matches!(
            change_outputs(&notes, u64::MAX, 0),
            Err(PoWError::RewardOverflow)
        ));
    }

    #[test]
    fn transfer_ops_groups_inputs_by_the_signing_key_limit() {
        // 70 notes -> groups of 32, 32, 6.
        let notes: Vec<NoteId> = (0u8..70).map(note_id).collect();
        let transfers = transfer_ops(&notes, ZkPublicKey::zero(), &[10, 20, 30]).unwrap();
        let input_counts: Vec<usize> = transfers
            .iter()
            .map(|t| (&t.inputs).into_iter().count())
            .collect();
        assert_eq!(input_counts, vec![32, 32, 6]);
    }

    #[test]
    fn push_reward_claim_ops_interleaves_claims_and_transfers() {
        let claims: Vec<_> = std::iter::repeat_with(|| ticket().1).take(40).collect();
        let notes: Vec<NoteId> = (0u8..40).map(note_id).collect();
        let changes = change_outputs(&notes, 100, 0).unwrap();
        let transfers = transfer_ops(&notes, ZkPublicKey::zero(), &changes).unwrap();

        let tx = push_reward_claim_ops(MantleTxBuilder::new(), &claims, transfers)
            .unwrap()
            .build()
            .unwrap();
        let ops = tx.op_refs();

        assert_eq!(ops.len(), 42); // 40 claims + 2 transfers
        assert!(
            ops[..32]
                .iter()
                .all(|op| matches!(op, OpRef::ClaimPowReward(_)))
        );
        assert!(matches!(ops[32], OpRef::Transfer(_)));
        assert!(
            ops[33..41]
                .iter()
                .all(|op| matches!(op, OpRef::ClaimPowReward(_)))
        );
        assert!(matches!(ops[41], OpRef::Transfer(_)));
    }

    #[test]
    fn estimate_reward_claim_fee_is_positive_with_nonzero_gas_prices() {
        let claims = vec![ticket().1];
        let notes = vec![note_id(0)];
        let fee =
            estimate_reward_claim_fee(&claims, &notes, ZkPublicKey::zero(), &context()).unwrap();
        assert!(fee > 0);
    }

    #[tokio::test]
    async fn build_reward_claim_tx_produces_a_claim_and_signed_transfer() {
        let (secret_key, claim) = ticket();
        let expected_note = Utxo::new(claim.op_id(), 0, Note::new(REWARD, claim.public_key)).id();

        let (tx, claimed) = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            POOL,
            GasPrices::new(1, 1),
            &[(secret_key, claim)],
        )
        .await
        .unwrap();

        assert_eq!(claimed, 1);
        assert_within_tx_limits(&tx);
        let ops = tx.op_refs();
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], OpRef::ClaimPowReward(_)));
        let OpRef::Transfer(transfer) = &ops[1] else {
            panic!("second op should be a transfer");
        };
        let inputs: Vec<NoteId> = (&transfer.inputs).into_iter().copied().collect();
        assert_eq!(inputs, vec![expected_note]);

        // A `None` proof for the claim, a `ZkSig` for the transfer.
        let proofs = tx.op_proof_refs();
        assert_eq!(proofs.len(), 2);
        assert!(matches!(proofs[0], OpProofRef::None(_)));
        assert!(matches!(proofs[1], OpProofRef::ZkSig(_)));
    }

    #[tokio::test]
    async fn build_reward_claim_tx_errors_when_rewards_disabled() {
        let result = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            0,
            POOL,
            GasPrices::new(1, 1),
            &[ticket()],
        )
        .await;
        assert!(matches!(result, Err(PoWError::RewardsDisabled)));
    }

    #[tokio::test]
    async fn build_reward_claim_tx_errors_when_pool_cannot_fund_a_claim() {
        // Pool below a single reward -> nothing fundable.
        let result = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            REWARD - 1,
            GasPrices::new(1, 1),
            &[ticket()],
        )
        .await;
        assert!(matches!(result, Err(PoWError::RewardPoolExhausted)));
    }

    #[tokio::test]
    async fn build_reward_claim_tx_caps_claims_to_the_reward_pool() {
        // Pool funds only two claims, but five tickets are offered.
        let tickets: Vec<_> = std::iter::repeat_with(ticket).take(5).collect();
        let (tx, claimed) = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            2 * REWARD,
            GasPrices::new(1, 1),
            &tickets,
        )
        .await
        .unwrap();

        assert_eq!(claimed, 2); // only two of the five tickets fit the pool
        assert_within_tx_limits(&tx);
        let ops = tx.op_refs();
        let claims = ops
            .iter()
            .filter(|op| matches!(op, OpRef::ClaimPowReward(_)))
            .count();
        assert_eq!(claims, 2); // 2 claims + 1 transfer
        assert_eq!(ops.len(), 3);
    }

    /// The drain loop spends against a running balance rather than the tip's
    /// pool, which cannot fall until earlier claims settle. Walking the same
    /// arithmetic the loop performs shows it converging instead of
    /// re-authorising the same funds every round.
    #[tokio::test]
    async fn successive_claims_draw_the_pool_down_to_exhaustion() {
        // Pool funds three claims in total; each round is capped at one because
        // only one ticket is offered at a time.
        let mut available_pool = 3 * REWARD;
        let mut rounds = 0;
        loop {
            let tickets = vec![ticket()];
            match build_reward_claim_tx_inner(
                ZkPublicKey::zero(),
                REWARD,
                available_pool,
                GasPrices::new(1, 1),
                &tickets,
            )
            .await
            {
                Ok((_, claimed)) => {
                    assert_eq!(claimed, 1);
                    available_pool -= claimed as u64 * REWARD;
                    rounds += 1;
                }
                // The pool is spent: this is how the drain loop learns to stop.
                Err(PoWError::RewardPoolExhausted) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
            assert!(rounds <= 3, "drawdown failed to converge");
        }
        assert_eq!(rounds, 3);
        assert_eq!(available_pool, 0);
    }

    /// Re-reading the tip each round — what the loop did before — never
    /// converges, because a published claim cannot change the tip until it
    /// settles. This pins the behaviour the drawdown exists to prevent.
    #[tokio::test]
    async fn a_pool_that_never_falls_would_authorise_claims_forever() {
        let stale_pool = 3 * REWARD;
        for _ in 0..10 {
            let tickets = vec![ticket()];
            let (_, claimed) = build_reward_claim_tx_inner(
                ZkPublicKey::zero(),
                REWARD,
                stale_pool,
                GasPrices::new(1, 1),
                &tickets,
            )
            .await
            .expect("a stale pool keeps funding claims");
            assert_eq!(claimed, 1);
        }
    }

    #[tokio::test]
    async fn build_reward_claim_tx_caps_claims_to_what_a_blend_payload_carries() {
        // More tickets and pool room than any cap allows. The payload budget
        // is the tightest of the three, so it is the one that binds, and the
        // resulting tx must fit the payload a claim is published in.
        let cap = *MAX_CLAIMS_BY_PAYLOAD_SIZE;
        assert!(
            cap < max_claims_by_ops(),
            "the payload budget is expected to bind before the op budget"
        );
        let tickets: Vec<_> = std::iter::repeat_with(ticket).take(cap + 50).collect();
        let (tx, claimed) = build_reward_claim_tx_inner(
            ZkPublicKey::zero(),
            REWARD,
            POOL, // POOL / REWARD = 1000 > cap, so the pool does not bind
            GasPrices::new(1, 1),
            &tickets,
        )
        .await
        .unwrap();

        assert_eq!(claimed, cap);
        assert_within_tx_limits(&tx);
        let ops = tx.op_refs();
        let claims = ops
            .iter()
            .filter(|op| matches!(op, OpRef::ClaimPowReward(_)))
            .count();
        let transfers = ops.len() - claims;
        assert_eq!(claims, cap);
        assert_eq!(transfers, cap.div_ceil(MAX_TRANSFER_INPUTS));
    }

    /// [`claim_tx_size`] measures a probe transaction rather than the one that
    /// is published, so the two shapes have to encode identically. This is
    /// what pins that: it compares the probe against really signed
    /// transactions at a single claim, a pair in one group, a full group, and
    /// the claim that opens the next one, so both the per-claim and per-group
    /// terms are covered.
    ///
    /// A failure here means the probe has drifted from what
    /// `build_reward_claim_tx_inner` builds — an op, a proof, or a transfer
    /// the real transaction carries and the probe does not, or vice versa.
    /// Fix the probe to match the builder; do not adjust the expected sizes,
    /// and do not relax this test. A probe that over-estimates needlessly
    /// shrinks every batch, and one that under-estimates puts the node back to
    /// building claims Blend refuses to carry.
    #[tokio::test]
    async fn claim_tx_size_matches_a_signed_transaction() {
        for claims in [1, 2, MAX_TRANSFER_INPUTS, MAX_TRANSFER_INPUTS + 1] {
            let tickets: Vec<_> = std::iter::repeat_with(ticket).take(claims).collect();
            let (tx, _) = build_reward_claim_tx_inner(
                ZkPublicKey::zero(),
                REWARD,
                POOL,
                GasPrices::new(1, 1),
                &tickets,
            )
            .await
            .unwrap();
            assert_eq!(
                tx.to_bytes().unwrap().len() as u64,
                claim_tx_size(claims).unwrap(),
                "probe disagrees with a signed tx at {claims} claim(s)"
            );
        }
    }

    #[test]
    fn the_payload_cap_is_the_largest_batch_a_payload_carries() {
        let claims = *MAX_CLAIMS_BY_PAYLOAD_SIZE;
        assert!(claim_tx_size(claims).unwrap() <= MAX_PAYLOAD_BODY_SIZE as u64);
        assert!(claim_tx_size(claims + 1).unwrap() > MAX_PAYLOAD_BODY_SIZE as u64);
    }

    /// A winning ticket anchored to a block at `block_slot`.
    fn winning_ticket(block_slot: u64) -> WinningTicket {
        let (secret_key, claim) = ticket();
        WinningTicket {
            tip: HeaderId::from([0u8; 32]),
            block_slot: Slot::new(block_slot),
            secret_key,
            claim,
        }
    }

    #[test]
    fn claimable_rewards_info_is_empty_without_tickets() {
        let info = claimable_rewards_info(&[], Slot::new(100), SLOT_WINDOW);
        assert_eq!(info.claimable_tickets, 0);
        assert!(info.slots_until_expiry.is_empty());
    }

    #[test]
    fn claimable_rewards_info_reports_remaining_window_per_ticket() {
        // SLOT_WINDOW is 100, so a block at slot S is claimable up to slot S +
        // 100.
        let tickets = [winning_ticket(50), winning_ticket(90)];
        let info = claimable_rewards_info(&tickets, Slot::new(100), SLOT_WINDOW);
        assert_eq!(info.claimable_tickets, 2);
        // (50 + 100) - 100 = 50, (90 + 100) - 100 = 90
        assert_eq!(info.slots_until_expiry, vec![Slot::new(50), Slot::new(90)]);
    }

    #[test]
    fn claimable_rewards_info_includes_the_last_valid_slot() {
        // A block at slot 50 is still claimable at exactly slot 150 (gap ==
        // window).
        let info = claimable_rewards_info(&[winning_ticket(50)], Slot::new(150), SLOT_WINDOW);
        assert_eq!(info.claimable_tickets, 1);
        assert_eq!(info.slots_until_expiry, vec![Slot::new(0)]);
    }

    #[test]
    fn prune_expired_tickets_drops_only_out_of_window_tickets() {
        // SLOT_WINDOW is 100. At slot 200: block 10 is expired (last claimable
        // 110), block 150 is still live (last claimable 250).
        let mut state = PoWServiceState {
            ready_to_claim: vec![winning_ticket(10), winning_ticket(150)],
            pending_to_claim: vec![winning_ticket(20), winning_ticket(180)],
        };
        prune_expired_tickets(&mut state, Slot::new(200), SLOT_WINDOW);

        let ready_slots: Vec<u64> = state
            .ready_to_claim
            .iter()
            .map(|t| u64::from(t.block_slot))
            .collect();
        let pending_slots: Vec<u64> = state
            .pending_to_claim
            .iter()
            .map(|t| u64::from(t.block_slot))
            .collect();
        assert_eq!(ready_slots, vec![150]);
        assert_eq!(pending_slots, vec![180]);
    }

    #[test]
    fn prune_expired_tickets_keeps_tickets_at_the_window_boundary() {
        // A block at slot 50 is still claimable at exactly slot 150 (gap ==
        // window).
        let mut state = PoWServiceState {
            ready_to_claim: vec![winning_ticket(50)],
            pending_to_claim: vec![],
        };
        prune_expired_tickets(&mut state, Slot::new(150), SLOT_WINDOW);
        assert_eq!(state.ready_to_claim.len(), 1);
    }
}
