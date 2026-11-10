pub mod api;
mod blend;
mod kms;
mod leadership;
mod mempool;
mod metrics;
mod relays;
mod tx_selection;
mod wallet;

use core::fmt::Debug;
use std::{
    fmt::Display,
    pin::{Pin, pin},
    sync::Arc,
    time::Duration,
};

use futures::{
    FutureExt as _, Stream, StreamExt as _,
    stream::{self, FuturesOrdered, FuturesUnordered},
};
use lb_blend_service::message::proposal_diagnostic_timestamp_unix_ms;
use lb_chain_network_service::api::{ChainNetworkServiceApi, ChainNetworkServiceData};
use lb_chain_service::{
    Epoch,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    block::{Block, BlockTransactions, Error as BlockError, UncleHeaders},
    header::HeaderId,
    mantle::{
        OpRef, SignedOps,
        ledger::verification_mode::StandardMode,
        traits::{Hashable, MantleTx, SignedMantleTx, StorageSize},
        transactions::{hash::TxHash, states::Preverified},
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate},
    sdp::blend::PolEpochState,
};
use lb_cryptarchia_engine::{Slot, average_slots_for_blocks};
use lb_key_management_system_service::{api::KmsServiceApi, keys::Ed25519Key};
use lb_ledger::{
    ConsensusExcludedKeys, LedgerState,
    config::{EraScheduledConfig, config_for_slot},
};
use lb_log_targets::{chain, diagnostic::BLEND_REACHABILITY};
use lb_services_utils::wait_until_services_are_ready;
use lb_storage_service::StorageService;
use lb_time_service::{SlotTick, TimeService, TimeServiceMessage, api::TimeServiceApi};
use lb_tx_service::{
    TxMempoolService,
    backend::{MemPool, RecoverableMempool},
    network::NetworkAdapter as MempoolNetworkAdapter,
    storage::MempoolStorageAdapter,
};
use lb_utils::tokio::task::spawn;
use lb_wallet_service::{
    UtxoWithKeyId,
    api::{WalletApi, WalletApiError},
};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, relay::OutboundRelay},
};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{Level, debug, error, info, instrument, span, trace};
use tracing_futures::Instrument as _;
use tx_selection::{TransactionSelection, select_transactions};

pub use crate::wallet::LeaderWalletConfig;
use crate::{
    blend::BlendAdapter,
    kms::PreloadKmsService,
    leadership::{
        SlotContext, build_proof_for, fetch_slot_context, search_for_winning_slots, winning_note,
    },
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
    relays::CryptarchiaConsensusRelays,
};

/// How many won slots wait for the proposal underway. Wins are far apart, so
/// these only fill up if proposals have stopped getting through.
const MAX_QUEUED_WON_SLOTS: usize = 4;

/// How many slots wait for their check before the next ones are skipped. A
/// check takes a fraction of a slot, so they only pile up if the wallet or the
/// KMS stalls, and one this far behind leaves its block little time anyway.
const MAX_PENDING_SLOT_CHECKS: usize = 16;

/// A slot this node won, on its way to a block.
struct WonSlot<Note> {
    slot: Slot,
    /// The slot the next block is due by, on average. Built any later, this
    /// slot's block would only come in late against it.
    deadline: Slot,
    /// The note that won it, which the block's proof is to be of.
    note: Note,
}

impl<Note> WonSlot<Note> {
    /// `slot`, won by `note`, with as long to build as a block takes on average
    /// in its era: `1/f` slots, `f` being the era's slot activation
    /// coefficient.
    fn new(slot: Slot, note: Note, era_schedule: &EraScheduledConfig) -> Self {
        let f = config_for_slot(era_schedule, slot)
            .consensus_config
            .slot_activation_coeff();
        // `1/f = denominator / numerator`, rounded down. No slot is won in an era
        // where `f` is zero.
        let average_block_slots = average_slots_for_blocks(1.try_into().unwrap(), f);
        Self {
            slot,
            deadline: Slot::from(u64::from(slot).saturating_add(average_block_slots.get())),
            note,
        }
    }
}

/// Builds and publishes the block of each won slot in turn, so that a win
/// builds on the block of the win before it rather than forking off it.
///
/// A build is given up on once `current_slot` reaches its deadline. Publishing
/// isn't, as a block should not be left applied but unpublished.
async fn propose_in_turn<Note, Block, Build, Built, Publish, Published>(
    mut won_slots: mpsc::Receiver<WonSlot<Note>>,
    mut current_slot: watch::Receiver<Slot>,
    mut build: Build,
    mut publish: Publish,
) where
    Build: FnMut(Slot, Note) -> Built,
    Built: Future<Output = Option<Block>>,
    Publish: FnMut(Block) -> Published,
    Published: Future<Output = ()>,
{
    while let Some(WonSlot {
        slot,
        deadline,
        note,
    }) = won_slots.recv().await
    {
        tokio::select! {
            built = build(slot, note) => {
                if let Some(block) = built {
                    publish(block).await;
                }
            }
            // False, and so never taken, if the clock stops first.
            true = async { current_slot.wait_for(|current| *current >= deadline).await.is_ok() } => {
                metrics::consensus_proposals_create_failed("deadline");
                error!(target: LOG_TARGET, "Giving up on the proposal for {slot:?}: past its deadline");
            }
        }
    }
}

fn log_sdp_activity_selected_for_proposal<Tx>(block: &Block<Tx>, ledger_state: &LedgerState)
where
    Tx: MantleTx,
{
    for (tx, active) in block.transactions_iter().flat_map(|tx| {
        tx.op_refs_iter().filter_map(move |op| match op {
            OpRef::SDPActive(active) => Some((tx, active)),
            _ => None,
        })
    }) {
        let provider_id = ledger_state
            .mantle_ledger()
            .sdp_ledger()
            .get_declaration(&active.declaration_id)
            .map(|declaration| declaration.provider_id);
        tracing::debug!(
            target: LOG_TARGET,
            diagnostic = BLEND_REACHABILITY,
            event = "sdp_activity_selected_for_proposal",
            tx_id = %tx.hash(),
            provider_id = ?provider_id,
            declaration_id = %active.declaration_id,
            proof_epoch = u32::from(active.metadata.origin_epoch()),
            proposal_block_id = %block.header().id(),
            proposal_slot = u64::from(block.header().slot()),
            "Selected SDP activity transaction for proposal"
        );
    }
}

/// The per-subscriber stream of per-epoch winning slots. Each item carries the
/// state used to construct its stream and that epoch's stream of winning slots.
///
/// `Send` but not `Sync`: each item carries a [`WinningPolSlotStream`] of
/// `Send`-only per-slot futures (see [`WinningSlotFuture`]), so the handoff is
/// not `Sync`.
pub type WinningPolEpochSlotsStream =
    Pin<Box<dyn Stream<Item = WinningPolEpochSlots> + Send + Unpin>>;

pub struct WinningPolEpochSlots {
    pub epoch: Epoch,
    pub state: PolEpochState,
    pub slots: WinningPolSlotStream,
}

/// A single slot's leadership-proof work: a future resolving to that slot's
/// leadership private inputs if it is a winning slot, or `None` otherwise.
///
/// Boxed `Send` (not `Sync`): the KMS adapter is `#[async_trait]`, so the
/// futures it awaits are `Send`-only, which makes this future `!Sync`.
pub type WinningSlotFuture = Pin<Box<dyn Future<Output = Option<LeaderPrivate>> + Send>>;

/// A lazy stream of one epoch's per-slot leadership-proof work: one
/// [`WinningSlotFuture`] per slot in the epoch's range. The consumer drives the
/// futures and filters out the non-winning (`None`) slots.
pub type WinningPolSlotStream = Pin<Box<dyn Stream<Item = WinningSlotFuture> + Send + Unpin>>;

/// Number of epochs to buffer for late subscribers to the winning `PoL` slots
/// stream. Subscribers will almost certainly consume each epoch at some point,
/// so `2` is already a safe value for the buffer.
const WINNING_POL_EPOCH_HANDOFF_BUFFER_SIZE: usize = 2;
const SERVICE_ID: &str = "ChainLeader";

pub(crate) const LOG_TARGET: &str = chain::leader::ROOT;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Ledger error: {0}")]
    Ledger(#[from] lb_ledger::LedgerError<HeaderId>),
    #[error("Consensus error: {0}")]
    Consensus(#[from] lb_cryptarchia_engine::Error<HeaderId>),
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Could not fetch block transactions: {0}")]
    FetchBlockTransactions(#[source] DynError),
    #[error("Failed to create valid block during proposal: {0}")]
    BlockCreation(#[from] BlockError),
    #[error("Wallet API error: {0}")]
    Wallet(#[from] Box<WalletApiError>),
    #[error("Mempool error: {0}")]
    Mempool(#[source] DynError),
    #[error("Chain service error: {0}")]
    ChainService(#[from] lb_chain_service::api::ApiError),
    #[error("Ledger state not found for {0:?}")]
    LedgerStateNotFound(HeaderId),
}

impl From<WalletApiError> for Error {
    fn from(error: WalletApiError) -> Self {
        Self::Wallet(Box::new(error))
    }
}

pub enum LeaderMsg {
    /// Subscribe to a stream of this node's winning `PoL` slots.
    ///
    /// The reply is a stream of per-epoch handoffs: one item per epoch, each
    /// carrying that epoch's lazy stream of winning slots. A dedicated
    /// background task scans the current epoch from the subscribe slot onward
    /// (and each later epoch in full).
    PotentialWinningPolEpochSlotStreamSubscribe {
        sender: oneshot::Sender<WinningPolEpochSlotsStream>,
    },
    Claim {
        sender: oneshot::Sender<Result<TxHash, Error>>,
    },
}

impl Debug for LeaderMsg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PotentialWinningPolEpochSlotStreamSubscribe { .. } => {
                f.write_str("PotentialWinningPolEpochSlotStreamSubscribe")
            }
            Self::Claim { .. } => f.write_str("Claim"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LeaderSettings {
    /// The ledger config of every era.
    pub era_schedule: Arc<EraScheduledConfig>,
    pub consensus_excluded_keys: ConsensusExcludedKeys,
    pub wallet_config: LeaderWalletConfig,
}

#[expect(clippy::allow_attributes_without_reason)]
pub struct CryptarchiaLeader<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    TimeBackend,
    CryptarchiaService,
    ChainNetwork,
    Wallet,
    RuntimeServiceId,
> where
    BlendService: lb_blend_service::ServiceComponents,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Settings: Clone,
    Mempool::Item: Clone + Eq + Debug + 'static,
    Mempool::Item: SignedMantleTx<Preverified, StandardMode>,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData,
    ChainNetwork: ChainNetworkServiceData,
    Wallet: lb_wallet_service::api::WalletServiceData,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    TimeBackend,
    CryptarchiaService,
    ChainNetwork,
    Wallet,
    RuntimeServiceId,
> ServiceData
    for CryptarchiaLeader<
        BlendService,
        Mempool,
        MempoolNetAdapter,
        TimeBackend,
        CryptarchiaService,
        ChainNetwork,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: lb_blend_service::ServiceComponents,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::Settings: Clone,
    Mempool::Item: SignedMantleTx<Preverified, StandardMode> + Clone + Eq + Debug,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData,
    ChainNetwork: ChainNetworkServiceData,
    Wallet: lb_wallet_service::api::WalletServiceData,
{
    type Settings = LeaderSettings;
    type State = overwatch::services::state::NoState<Self::Settings>;
    type StateOperator = overwatch::services::state::NoOperator<Self::State>;
    type Message = LeaderMsg;
}

#[async_trait::async_trait]
impl<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    TimeBackend,
    CryptarchiaService,
    ChainNetwork,
    Wallet,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for CryptarchiaLeader<
        BlendService,
        Mempool,
        MempoolNetAdapter,
        TimeBackend,
        CryptarchiaService,
        ChainNetwork,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: ServiceData<
            Message = lb_blend_service::message::ProxyServiceMessage<
                lb_blend_service::message::ServiceMessage<BlendService::NodeId>,
            >,
        > + lb_blend_service::ServiceComponents<NodeId: Send + Sync>
        + Send
        + 'static,
    Mempool: MemPool<Item = SignedOps<Preverified, StandardMode>>
        + RecoverableMempool<BlockId = HeaderId, Key = TxHash>
        + Send
        + Sync
        + 'static,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Item: Hashable<Hash = Mempool::Key>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Mempool::Item: SignedMantleTx<Preverified, StandardMode>,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend + 'static,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item> + 'static,
    ChainNetwork: ChainNetworkServiceData<Tx = Mempool::Item>,
    Wallet: lb_wallet_service::api::WalletServiceData + 'static,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<BlendService>
        + AsServiceId<StorageService<RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetAdapter,
                Mempool,
                Mempool::Storage,
                TimeBackend,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<CryptarchiaService>
        + AsServiceId<ChainNetwork>
        + AsServiceId<Wallet>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
        })
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    async fn run(mut self) -> Result<(), DynError> {
        // Create the API wrapper for chain service communication
        let cryptarchia_api = CryptarchiaServiceApi::<CryptarchiaService>::from_overwatch_handle(
            &self.service_resources_handle.overwatch_handle,
        )
        .await;

        // Wait for other services to become ready, with timeout.
        // (except Chain, ChainNetwork, Blend, and Wallet)
        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_mins(1)),
            TxMempoolService<_, _, _, _, _>,
            TimeService<_, _>,
            PreloadKmsService<_>
        )
        .await?;
        // Wait for the remaining dependencies to become ready, without timeout
        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            None,
            CryptarchiaService, // becomes ready after recoverying blocks
            ChainNetwork,       // becomes ready after IBD
            BlendService,       // becomes ready after chain becomes online
            Wallet              // becomes ready after Chain
        )
        .await?;

        // Wait until the chain becomes Online mode.
        // We should not propose blocks while the chain is in Bootstrapping mode.
        info!(target: LOG_TARGET, "Waiting for chain to become online");
        cryptarchia_api
            .wait_until_chain_becomes_online()
            .await
            .expect("Waiting for chain to be online should succeed");
        info!(target: LOG_TARGET, "Chain is online. Starting block proposals.");

        // Only the chain service is needed before the chain is online: the
        // rest is set up once the leader starts proposing.
        let mut relays: CryptarchiaConsensusRelays<
            BlendService,
            Mempool,
            MempoolNetAdapter,
            RuntimeServiceId,
        > = CryptarchiaConsensusRelays::from_service_resources_handle::<
            Self,
            TimeBackend,
            CryptarchiaService,
        >(&self.service_resources_handle)
        .await;

        let chain_network_api = ChainNetworkServiceApi::<ChainNetwork>::new(
            self.service_resources_handle
                .overwatch_handle
                .relay::<ChainNetwork>()
                .await
                .expect("Failed to estabilish connection with ChainNetwork"),
        );

        let LeaderSettings {
            era_schedule,
            consensus_excluded_keys,
            wallet_config,
        } = self
            .service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let wallet_api = WalletApi::<Wallet, RuntimeServiceId>::new(
            self.service_resources_handle
                .overwatch_handle
                .relay::<Wallet>()
                .await?,
        );

        let kms_api = KmsServiceApi::<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>::new(
            self.service_resources_handle
                .overwatch_handle
                .relay::<PreloadKmsService<_>>()
                .await
                .expect("Relay with KMS service should be available."),
        );

        let (mut current_era, current_slot, mut slot_timer, mut era_transition_ends) = {
            let time_service_api =
                TimeServiceApi::<TimeService<TimeBackend, RuntimeServiceId>>::new(
                    &self.service_resources_handle.overwatch_handle,
                )
                .await?;
            let (current_slot_tick, slot_timer) =
                time_service_api.subscribe_to_epoch_slots().await?;
            let era_transition_ends = time_service_api.subscribe_to_era_transition_ends().await?;
            (
                current_slot_tick.era,
                current_slot_tick.slot,
                slot_timer,
                era_transition_ends,
            )
        };

        // Proposals take seconds of proving, so the loop polls the one underway
        // along with its other inputs, to see every slot.
        let (won_slots, won_slots_rx) = mpsc::channel(MAX_QUEUED_WON_SLOTS);
        let (current_slot, current_slot_rx) = watch::channel(current_slot);
        // A copy, as the loop needs the relays' adapter mutably at era changes.
        let mempool = relays.mempool_adapter().clone();
        let blend_adapter = BlendAdapter::<BlendService>::new(relays.blend_relay().clone());
        let mut proposing = pin!(
            propose_in_turn(
                won_slots_rx,
                current_slot_rx,
                |slot, note| {
                    Self::build_block_for(
                        slot,
                        note,
                        &cryptarchia_api,
                        &wallet_api,
                        &kms_api,
                        &mempool,
                        &era_schedule,
                        &consensus_excluded_keys,
                    )
                },
                |block| {
                    Self::apply_and_publish_block_proposal(
                        block,
                        &chain_network_api,
                        &blend_adapter,
                    )
                },
            )
            .fuse()
        );
        // Building a claim's transaction proves too, so the loop polls claims
        // along with its other inputs as well.
        let mut claims = FuturesUnordered::new();
        // Checking a slot for a win takes the wallet and the KMS, so the loop
        // polls the checks too, and takes the next tick on time whatever they
        // take. They come back in slot order, the order to build in.
        let mut slot_checks = FuturesOrdered::new();

        self.service_resources_handle.status_updater.notify_ready();
        info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        let async_loop = async {
            loop {
                tokio::select! {
                    Some(SlotTick { era, slot, epoch }) = slot_timer.next() => {
                        trace!(target: LOG_TARGET, "Received SlotTick for slot {}, ep {}", u64::from(slot), u32::from(epoch));
                        current_slot.send_replace(slot);
                        if era > current_era {
                            relays.mempool_adapter_mut().rotate_era(era).await;
                            current_era = era;
                        }
                        if slot_checks.len() < MAX_PENDING_SLOT_CHECKS {
                            slot_checks.push_back(Self::check_slot(slot, &cryptarchia_api, &wallet_api, &kms_api, &consensus_excluded_keys));
                        } else {
                            metrics::consensus_proposals_create_failed("slot_checks_queue_full");
                            error!(target: LOG_TARGET, "Too many slots waiting for their check: skipping {slot:?}");
                        }
                    }

                    Some((slot, note)) = slot_checks.next() => {
                        if let Some(note) = note
                            && won_slots.try_send(WonSlot::new(slot, note, &era_schedule)).is_err()
                        {
                            metrics::consensus_proposals_create_failed("won_slots_queue_full");
                            error!(target: LOG_TARGET, "Too many proposals underway: giving up on {slot:?}");
                        }
                    }

                    Some(transition_end) = era_transition_ends.next() => {
                        relays.mempool_adapter_mut().complete_era_transition(transition_end.era()).await;
                    }

                    Some(msg) = self.service_resources_handle.inbound_relay.next() => {
                        Self::handle_inbound_message(msg, &cryptarchia_api, &wallet_api, &kms_api, relays.time_relay(), &era_schedule, &consensus_excluded_keys, &wallet_config, &mempool, &claims);
                    }

                    () = &mut proposing => {}

                    Some(()) = claims.next() => {}
                }
            }
        };

        // It sucks to use `SERVICE_ID` when we have `<RuntimeServiceId as
        // AsServiceId<Self>>::SERVICE_ID`.
        // Somehow it just does not let us use it.
        //
        // Hypothesis:
        // 1. Probably related to too many generics.
        // 2. It seems `span` requires a `const` string literal.
        async_loop
            .instrument(span!(target: LOG_TARGET, Level::TRACE, SERVICE_ID))
            .await;

        Ok(())
    }
}

impl<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    TimeBackend,
    CryptarchiaService,
    ChainNetwork,
    Wallet,
    RuntimeServiceId,
>
    CryptarchiaLeader<
        BlendService,
        Mempool,
        MempoolNetAdapter,
        TimeBackend,
        CryptarchiaService,
        ChainNetwork,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: ServiceData<
            Message = lb_blend_service::message::ProxyServiceMessage<
                lb_blend_service::message::ServiceMessage<BlendService::NodeId>,
            >,
        > + lb_blend_service::ServiceComponents<NodeId: Send + Sync>
        + Send
        + 'static,
    Mempool: MemPool<Item = SignedOps<Preverified, StandardMode>>
        + RecoverableMempool<BlockId = HeaderId, Key = TxHash>
        + Send
        + Sync
        + 'static,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Item: SignedMantleTx<Preverified, StandardMode, Hash = Mempool::Key>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    <Mempool as MemPool>::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend + 'static,
    TimeBackend::Settings: Clone + Send + Sync,
    CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item> + 'static,
    ChainNetwork: ChainNetworkServiceData<Tx = Mempool::Item>,
    Wallet: lb_wallet_service::api::WalletServiceData + 'static,
    RuntimeServiceId: Debug
        + Display
        + Sync
        + Send
        + 'static
        + AsServiceId<Wallet>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>,
{
    /// `slot`, with the note of the node's that won it, if any.
    async fn check_slot(
        slot: Slot,
        cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
        wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
        kms_api: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
        consensus_excluded_keys: &ConsensusExcludedKeys,
    ) -> (Slot, Option<UtxoWithKeyId>) {
        let Some(SlotContext {
            epoch_state,
            eligible_aged,
            ..
        }) = fetch_slot_context(cryptarchia_api, wallet_api, consensus_excluded_keys, slot).await
        else {
            error!(target: LOG_TARGET, "Failed to fetch epoch context for slot {slot:?}");
            return (slot, None);
        };
        let note = winning_note(&eligible_aged, &epoch_state, slot, kms_api).await;
        (slot, note)
    }

    /// Builds the block of `slot`, which `note` won, on the current tip, if
    /// any.
    #[expect(
        clippy::too_many_arguments,
        reason = "Everything a block of the node's is built from"
    )]
    async fn build_block_for(
        slot: Slot,
        note: UtxoWithKeyId,
        cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
        wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
        kms_api: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
        mempool: &MempoolAdapter<Mempool::Item>,
        era_schedule: &EraScheduledConfig,
        consensus_excluded_keys: &ConsensusExcludedKeys,
    ) -> Option<Block<Mempool::Item>> {
        if Self::tip_has_passed(slot, cryptarchia_api).await {
            return None;
        }

        let Some(SlotContext {
            wallet_tip,
            epoch_state,
            ..
        }) = fetch_slot_context(cryptarchia_api, wallet_api, consensus_excluded_keys, slot).await
        else {
            error!(target: LOG_TARGET, "Failed to fetch epoch context for slot {slot:?}");
            return None;
        };
        let tip_state = Self::tip_state(wallet_tip, cryptarchia_api).await?;

        let latest_tree = tip_state.latest_utxos();
        let proof = match build_proof_for(
            &note,
            latest_tree,
            &epoch_state,
            slot,
            wallet_api,
            kms_api,
        )
        .await
        {
            Ok(proof) => proof,
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    diagnostic = BLEND_REACHABILITY,
                    event = "leadership_proof_failure",
                    epoch = u32::from(era_schedule.epoch_for_slot(slot)),
                    slot = u64::from(slot),
                    error = %e,
                    "Failed to build leadership proof"
                );
                return None;
            }
        };

        let (proof, signing_key) = proof?;
        Self::propose_block(
            wallet_tip,
            slot,
            proof,
            &signing_key,
            cryptarchia_api,
            mempool,
            tip_state,
            era_schedule,
        )
        .await
        .map_err(|e| {
            metrics::consensus_proposals_create_failed("propose_block");
            error!(target: LOG_TARGET, "{e}");
        })
        .ok()
    }

    /// Whether the tip is at or past `slot`, as it can get while the slot waits
    /// its turn. A block has to come after its parent, and the epoch state of a
    /// slot behind the tip can't be fetched, so this is to be asked first.
    async fn tip_has_passed(
        slot: Slot,
        cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
    ) -> bool {
        let Ok(info) = cryptarchia_api.info().await else {
            return false;
        };
        let tip_slot = info.cryptarchia_info.slot;
        if tip_slot < slot {
            return false;
        }
        debug!(target: LOG_TARGET, ?slot, ?tip_slot, "Skipping a won slot the tip has passed");
        true
    }

    /// The ledger state at `tip`, which the proof of a win needs to show that
    /// the winning note is still unspent.
    async fn tip_state(
        tip: HeaderId,
        cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
    ) -> Option<LedgerState> {
        match cryptarchia_api.get_ledger_state(tip).await {
            Ok(Some(state)) => Some(state),
            Ok(None) => {
                error!(target: LOG_TARGET, "Ledger state not found for tip {tip:?}");
                None
            }
            Err(e) => {
                error!(target: LOG_TARGET, "Failed to get ledger state for tip: {e}");
                None
            }
        }
    }

    #[instrument(
        target = LOG_TARGET,
        level = "debug",
        skip(
            mempool,
            ledger_state,
            era_schedule,
            cryptarchia_api,
            proof,
            signing_key
        )
    )]
    #[expect(clippy::too_many_arguments, reason = "Need all args")]
    async fn propose_block(
        parent: HeaderId,
        slot: Slot,
        proof: Groth16LeaderProof,
        signing_key: &Ed25519Key,
        cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
        mempool: &MempoolAdapter<Mempool::Item>,
        mut ledger_state: LedgerState,
        era_schedule: &EraScheduledConfig,
    ) -> Result<Block<Mempool::Item>, Error> {
        let txs_stream = mempool
            .get_mempool_view([0; 32].into())
            .await
            .map_err(Error::FetchBlockTransactions)?;

        let tx_stream: Pin<Box<_>> = Box::pin(txs_stream);

        let uncle_headers = cryptarchia_api
            .select_uncles(parent, slot)
            .await
            .unwrap_or_else(|err| {
                error!(target: LOG_TARGET, ?slot, %err, "failed to select uncles");
                // A proposal without uncles is still valid
                UncleHeaders::empty()
            });

        (ledger_state, _) = ledger_state
            .clone()
            .try_apply_header::<Groth16LeaderProof, HeaderId>(
                slot,
                &proof,
                &uncle_headers.slots(),
                era_schedule,
            )?;
        // Collect all candidate transactions up front so the ones that fail can
        // be retried across multiple rounds.
        let TransactionSelection {
            ledger_state,
            selected_txs,
            invalid_tx_hashes,
        } = select_transactions(ledger_state, tx_stream.collect().await, era_schedule);

        if !invalid_tx_hashes.is_empty()
            && let Err(e) = mempool.remove_transactions(&invalid_tx_hashes).await
        {
            error!(target: LOG_TARGET, "Failed to remove invalid transactions from mempool: {e:?}");
        }

        let valid_tx_stream = stream::iter(selected_txs);
        let config = config_for_slot(era_schedule, slot);
        let txs = txs_for_block(
            valid_tx_stream,
            config.max_transactions,
            config.max_transactions_size,
        )
        .await;

        let block = Block::create(
            parent,
            slot,
            uncle_headers,
            proof,
            txs,
            config.max_transactions_size,
            signing_key,
        )?;
        debug!(
            target: LOG_TARGET,
            event = "leader_proposal_created",
            timestamp_unix_ms = proposal_diagnostic_timestamp_unix_ms(),
            block_id = %block.header().id(),
            slot = u64::from(block.header().slot()),
            "Created local block proposal"
        );
        if tracing::enabled!(Level::DEBUG) {
            log_sdp_activity_selected_for_proposal(&block, &ledger_state);
        }

        info!(
            target: LOG_TARGET,
            "proposed block {:?} with {} transactions ({} removed)",
            block.header().id(),
            block.transactions_iter().len(),
            invalid_tx_hashes.len()
        );

        Ok(block)
    }

    /// Apply our own proposed block to the chain and publish it to the blend
    /// network.
    async fn apply_and_publish_block_proposal(
        block: Block<Mempool::Item>,
        chain_network_api: &ChainNetworkServiceApi<ChainNetwork>,
        blend_adapter: &BlendAdapter<BlendService>,
    ) {
        if let Err(e) = chain_network_api
            .apply_block_and_reconcile_mempool(block.clone())
            .await
        {
            error!(target: LOG_TARGET, "Failed to apply our own proposed block {:?}: {e:?}", block.header().id());
            return;
        }

        #[cfg(not(feature = "testing-disable-proposal-publish"))]
        blend_adapter.publish_proposal(block.to_proposal()).await;
        #[cfg(feature = "testing-disable-proposal-publish")]
        let _ = {
            tracing::warn!(target: LOG_TARGET, "proposal publishing is disabled by the `testing-disable-proposal-publish` feature");
            blend_adapter
        };

        metrics::consensus_proposals_created_local();
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "All handles are required to serve claims and the winning-slot subscription."
    )]
    fn handle_inbound_message<'a>(
        msg: LeaderMsg,
        cryptarchia: &'a CryptarchiaServiceApi<CryptarchiaService>,
        wallet: &'a WalletApi<Wallet, RuntimeServiceId>,
        kms: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
        time_relay: &OutboundRelay<TimeServiceMessage>,
        era_schedule: &Arc<EraScheduledConfig>,
        consensus_excluded_keys: &ConsensusExcludedKeys,
        config: &'a LeaderWalletConfig,
        mempool: &'a MempoolAdapter<Mempool::Item>,
        claims: &FuturesUnordered<Pin<Box<dyn Future<Output = ()> + Send + 'a>>>,
    ) {
        match msg {
            LeaderMsg::Claim { sender } => {
                claims.push(Box::pin(Self::handle_claim_message(
                    cryptarchia,
                    wallet,
                    config,
                    mempool,
                    sender,
                )));
            }
            LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { sender } => {
                // Spawn a dedicated, off-main-loop task that scans winning slots
                // for this subscriber and streams them in, with the bounded
                // channel providing backpressure.
                let (epoch_handoff_sender, epoch_handoff_receiver) =
                    mpsc::channel(WINNING_POL_EPOCH_HANDOFF_BUFFER_SIZE);
                spawn(
                    "logos/chain/winning-slot-scanner",
                    search_for_winning_slots(
                        (*cryptarchia).clone(),
                        (*wallet).clone(),
                        (*kms).clone(),
                        (*time_relay).clone(),
                        Arc::clone(era_schedule),
                        consensus_excluded_keys.clone(),
                        epoch_handoff_sender,
                    ),
                );
                let stream: WinningPolEpochSlotsStream =
                    Box::pin(ReceiverStream::new(epoch_handoff_receiver));
                if sender.send(stream).is_err() {
                    error!(target: LOG_TARGET, "Could not send winning PoL epoch slots stream to subscriber.");
                }
            }
        }
    }

    async fn handle_claim_message(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        config: &LeaderWalletConfig,
        mempool: &MempoolAdapter<Mempool::Item>,
        resp_tx: oneshot::Sender<Result<TxHash, Error>>,
    ) {
        let result = Self::build_and_submit_claim_tx(cryptarchia, wallet, mempool, config).await;
        if resp_tx.send(result).is_err() {
            error!(target: LOG_TARGET, "Failed to send claim response");
        }
    }

    async fn build_and_submit_claim_tx(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        mempool: &MempoolAdapter<Mempool::Item>,
        config: &LeaderWalletConfig,
    ) -> Result<TxHash, Error> {
        let (tip, ledger_state) = Self::get_tip_ledger_state(cryptarchia).await?;

        let reward_amount = ledger_state.mantle_ledger().leader_reward_amount();
        let signed_tx = wallet
            .build_leader_claim_tx(
                tip,
                *ledger_state.mantle_ledger().vouchers_snapshot_root(),
                reward_amount,
                config.funding_pk,
                config.max_tx_fee,
            )
            .await?
            .response;
        let tx_hash = signed_tx.hash();

        mempool.post_tx(signed_tx).await.map_err(Error::Mempool)?;
        Ok(tx_hash)
    }

    async fn get_tip_ledger_state(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService>,
    ) -> Result<(HeaderId, LedgerState), Error> {
        let tip = cryptarchia.info().await?.cryptarchia_info.tip;
        let ledger_state = cryptarchia
            .get_ledger_state(tip)
            .await?
            .ok_or(Error::LedgerStateNotFound(tip))?;
        Ok((tip, ledger_state))
    }
}

/// Select transactions for a block, truncating the stream at the first
/// transaction that trips the era's limits: `max_transactions` transactions,
/// taking at most `max_transactions_size` bytes.
async fn txs_for_block<Tx, S>(
    mut txs: S,
    max_transactions: u16,
    max_transactions_size: u32,
) -> BlockTransactions<Tx>
where
    Tx: StorageSize,
    S: Stream<Item = Tx> + Unpin,
{
    let mut block_transactions_size: usize = 0;
    let mut selected_txs = Vec::new();

    while selected_txs.len() < usize::from(max_transactions) {
        let Some(tx) = txs.next().await else {
            break;
        };

        let tx_size = tx.storage_size();
        let Some(next_block_transactions_size) = block_transactions_size.checked_add(tx_size)
        else {
            break;
        };

        if next_block_transactions_size > max_transactions_size as usize {
            break;
        }

        selected_txs.push(tx);
        block_transactions_size = next_block_transactions_size;
    }

    BlockTransactions::try_new(selected_txs, max_transactions)
        .expect("No more transactions than the era allows are selected.")
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        num::NonZero,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use lb_utils::math::NonNegativeRatio;
    use tokio::{
        task::yield_now,
        time::{sleep, timeout},
    };

    use super::*;

    /// A deadline no test gets to.
    const FAR_OFF: u64 = u64::MAX;

    /// A slot won, to be built by the slot `deadline`.
    fn won(slot: u64, deadline: u64) -> WonSlot<()> {
        WonSlot {
            slot: Slot::new(slot),
            deadline: Slot::new(deadline),
            note: (),
        }
    }

    /// The slots won, one after the other, with no more to come.
    fn won_slots(slots: impl IntoIterator<Item = WonSlot<()>>) -> mpsc::Receiver<WonSlot<()>> {
        let (won_slots, won_slots_rx) = mpsc::channel(MAX_QUEUED_WON_SLOTS);
        for won_slot in slots {
            assert!(won_slots.try_send(won_slot).is_ok());
        }
        won_slots_rx
    }

    #[tokio::test]
    async fn won_slots_are_proposed_one_at_a_time_and_in_order() {
        let building = &AtomicUsize::new(0);
        let published = &Mutex::new(Vec::new());
        let (_clock, current_slot) = watch::channel(Slot::genesis());

        propose_in_turn(
            won_slots([won(1, FAR_OFF), won(2, FAR_OFF), won(3, FAR_OFF)]),
            current_slot,
            async move |slot, ()| {
                assert_eq!(building.fetch_add(1, Ordering::SeqCst), 0);
                sleep(Duration::from_millis(1)).await;
                building.fetch_sub(1, Ordering::SeqCst);
                Some(slot)
            },
            async move |slot| published.lock().unwrap().push(slot),
        )
        .await;

        assert_eq!(
            *published.lock().unwrap(),
            [Slot::new(1), Slot::new(2), Slot::new(3)]
        );
    }

    #[tokio::test]
    async fn a_build_past_its_deadline_gives_way_to_the_next_won_slot() {
        let published = &Mutex::new(Vec::new());
        let (stuck, on_time) = (Slot::new(1), Slot::new(2));
        let (clock, current_slot) = watch::channel(Slot::genesis());

        let proposing = propose_in_turn(
            won_slots([won(1, 2), won(2, FAR_OFF)]),
            current_slot,
            move |slot, ()| {
                let build_is_stuck = slot == stuck;
                async move {
                    if build_is_stuck {
                        pending::<()>().await;
                    }
                    Some(slot)
                }
            },
            async move |slot| published.lock().unwrap().push(slot),
        );
        let deadline_comes = async {
            yield_now().await;
            clock.send_replace(Slot::new(2));
        };
        timeout(Duration::from_secs(10), async {
            tokio::join!(proposing, deadline_comes)
        })
        .await
        .expect("The stuck build should give way at its deadline.");

        assert_eq!(*published.lock().unwrap(), [on_time]);
    }

    #[tokio::test]
    async fn the_deadline_leaves_publishing_alone() {
        let published = &Mutex::new(Vec::new());
        let (clock, current_slot) = watch::channel(Slot::genesis());
        let clock = &clock;

        propose_in_turn(
            won_slots([won(1, 2)]),
            current_slot,
            async |slot, ()| Some(slot),
            async move |slot| {
                clock.send_replace(Slot::new(2));
                sleep(Duration::from_millis(10)).await;
                published.lock().unwrap().push(slot);
            },
        )
        .await;

        assert_eq!(*published.lock().unwrap(), [Slot::new(1)]);
    }

    /// A single era, whose slot activation coefficient is `f`.
    fn era_schedule_with(f: NonNegativeRatio) -> EraScheduledConfig {
        leadership::single_era(lb_ledger::Config {
            consensus_config: lb_cryptarchia_engine::Config::new(
                NonZero::new(5).unwrap(),
                f,
                1f64.try_into().unwrap(),
                12,
                4,
            ),
            ..leadership::test_config()
        })
    }

    #[test]
    fn a_won_slot_has_until_the_next_block_is_due_on_average() {
        let f = |numerator, denominator: u32| {
            NonNegativeRatio::new(numerator, denominator.try_into().unwrap())
        };

        // `1/f` slots, rounded down.
        for (f, deadline) in [(f(1, 10), 17), (f(3, 10), 10)] {
            let won_slot = WonSlot::new(Slot::new(7), (), &era_schedule_with(f));
            assert_eq!(won_slot.deadline, Slot::new(deadline));
        }
    }

    #[derive(Clone)]
    struct TestTx {
        size: usize,
    }

    impl StorageSize for TestTx {
        fn storage_size(&self) -> usize {
            self.size
        }
    }

    /// The most transactions the tests' blocks may hold.
    const MAX_TRANSACTIONS: u16 = 1024;
    /// The most bytes the tests' blocks' transactions may take.
    const MAX_TRANSACTIONS_SIZE: u32 = 2 * 1024 * 1024;
    /// [`MAX_TRANSACTIONS_SIZE`], as transaction sizes are counted.
    const MAX_SIZE: usize = MAX_TRANSACTIONS_SIZE as usize;

    #[tokio::test]
    async fn block_tx_selection_respects_transaction_count_limit() {
        let txs = stream::iter(vec![TestTx { size: 1 }; usize::from(MAX_TRANSACTIONS) + 1]);

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE).await;

        assert_eq!(selected.len(), usize::from(MAX_TRANSACTIONS));
    }

    #[tokio::test]
    async fn block_tx_selection_respects_block_size_limit() {
        let txs = stream::iter(vec![
            TestTx { size: MAX_SIZE / 2 },
            TestTx { size: MAX_SIZE / 2 },
            TestTx { size: 1 },
        ]);

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE).await;
        let selected_size: usize = selected.iter().map(StorageSize::storage_size).sum();

        assert_eq!(selected.len(), 2);
        assert_eq!(selected_size, MAX_SIZE);
    }

    #[tokio::test]
    async fn block_tx_selection_stops_at_first_transaction_that_does_not_fit() {
        // The middle transaction does not fit alongside the first, so selection
        // must stop there and must not pull the third (which would fit on its
        // own) ahead of it — doing so could drop a dependency of the third.
        let txs = stream::iter(vec![
            TestTx { size: 10 },
            TestTx { size: MAX_SIZE },
            TestTx { size: 10 },
        ]);

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE).await;

        assert_eq!(selected.len(), 1);
        assert_eq!(selected.as_slice()[0].storage_size(), 10);
    }

    #[tokio::test]
    async fn block_tx_selection_stops_at_leading_oversized_transaction() {
        // A transaction larger than the whole block can never fit. Selection
        // stops at it rather than skipping past to later transactions, which may
        // depend on it. (In practice such transactions are filtered out before
        // reaching here, but the prefix invariant must hold regardless.)
        let txs = stream::iter(vec![TestTx { size: MAX_SIZE + 1 }, TestTx { size: 1 }]);

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE).await;

        assert!(selected.is_empty());
    }
}
