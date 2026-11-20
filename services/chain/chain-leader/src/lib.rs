pub mod api;
mod blend;
mod kms;
mod leadership;
mod mempool;
mod metrics;
mod proposal;
mod relays;
mod tx_selection;
mod wallet;

use core::fmt::Debug;
use std::{
    fmt::Display,
    pin::{Pin, pin},
    sync::Arc,
};

use futures::{FutureExt as _, Stream, StreamExt as _};
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
        Note, OpRef, SignedOps,
        ledger::verification_mode::StandardMode,
        traits::{Hashable, MantleTx, SignedMantleTx, StorageSize},
        transactions::{hash::TxHash, states::Preverified},
    },
    proofs::leader_proof::{Error as LeaderProofError, LeaderPrivate},
    sdp::blend::PolEpochState,
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_service::{api::KmsServiceApi, keys::ZkPublicKey};
use lb_ledger::{
    LedgerState,
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
use lb_utils::tokio::task::{spawn, spawn_blocking};
use lb_wallet_service::api::{WalletApi, WalletApiError};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, relay::OutboundRelay},
};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot, watch},
    task::JoinError,
};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tracing::{Level, debug, error, info, instrument, span, trace};
use tracing_futures::Instrument as _;
use tx_selection::{TransactionSelection, block_leader_reward, select_transactions};

pub use crate::wallet::LeaderWalletConfig;
use crate::{
    blend::BlendAdapter,
    kms::{KmsAdapter as _, PreloadKmsService},
    leadership::{
        PrivateInputsError, SlotContext, WinningNote, fetch_slot_context, find_winning_note,
        search_for_winning_slots,
    },
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
    proposal::{assemble_block, block_msg},
    relays::CryptarchiaConsensusRelays,
};

/// How many slots wait for the one underway before the next ones are skipped. A
/// slot that doesn't win takes a fraction of a slot, so they only pile up
/// behind a proof, or if the wallet or the KMS stalls, and one this far behind
/// leaves its block little time anyway.
const MAX_QUEUED_SLOTS: usize = 16;

/// Checks each slot for a win and builds and publishes its block, in turn, so
/// that a win builds on the block of the win before it rather than forking off
/// it.
///
/// A build is given up on once `tip_slot` has moved past its slot, as its block
/// would only come in behind the chain. A block of the same slot doesn't stop
/// it: fork choice settles between the two. Publishing isn't given up on, as a
/// block should not be left applied but unpublished.
async fn propose_in_turn<Block, Build, Built, Publish, Published>(
    mut slots: mpsc::Receiver<Slot>,
    mut tip_slot: watch::Receiver<Slot>,
    mut build: Build,
    mut publish: Publish,
) where
    Build: FnMut(Slot) -> Built,
    Built: Future<Output = Option<Block>>,
    Publish: FnMut(Block) -> Published,
    Published: Future<Output = ()>,
{
    while let Some(slot) = slots.recv().await {
        tokio::select! {
            built = build(slot) => {
                if let Some(block) = built {
                    publish(block).await;
                }
            }
            // False, and so never taken, if the tip stops being followed first.
            true = async { tip_slot.wait_for(|tip| *tip > slot).await.is_ok() } => {
                metrics::consensus_proposals_create_failed("tip_passed");
                error!(target: LOG_TARGET, "Giving up on the proposal for {slot:?}: the tip has moved past it");
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
    #[error("Private input generation failed: {0}")]
    PrivateInputs(#[from] PrivateInputsError),
    #[error("Proof generation failed: {0}")]
    Proof(#[from] LeaderProofError),
    #[error("Proof generation task failed: {0}")]
    ProofTask(#[from] JoinError),
    #[error("Transaction selection task failed: {0}")]
    SelectionTask(#[source] JoinError),
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
}

impl Debug for LeaderMsg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PotentialWinningPolEpochSlotStreamSubscribe { .. } => {
                f.write_str("PotentialWinningPolEpochSlotStreamSubscribe")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct LeaderSettings {
    /// The ledger config of every era.
    pub era_schedule: Arc<EraScheduledConfig>,
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
    TimeBackend: lb_time_service::backends::TimeBackend,
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

        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            None,
            TxMempoolService<_, _, _, _, _>,
            TimeService<_, _>,
            PreloadKmsService<_>,
            CryptarchiaService,
            ChainNetwork,
            BlendService,
            Wallet
        )
        .await?;

        // Wait until the chain becomes Online mode.
        // We should not propose blocks while the chain is in Bootstrapping
        // mode.
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

        let (mut current_era, mut slot_timer, mut era_transition_ends) = {
            let time_service_api =
                TimeServiceApi::<TimeService<TimeBackend, RuntimeServiceId>>::new(
                    &self.service_resources_handle.overwatch_handle,
                )
                .await?;
            let (current_slot_tick, slot_timer) =
                time_service_api.subscribe_to_epoch_slots().await?;
            let era_transition_ends = time_service_api.subscribe_to_era_transition_ends().await?;
            (current_slot_tick.era, slot_timer, era_transition_ends)
        };

        // Proposals take seconds of proving, so the loop polls the one underway
        // along with its other inputs, to see every slot.
        let (slots, slots_rx) = mpsc::channel(MAX_QUEUED_SLOTS);
        // The slot of the tip, for a build to stop once the tip has moved past
        // its slot. A build checks the tip as it starts, so genesis will do
        // until a block comes in.
        let mut new_blocks = BroadcastStream::new(cryptarchia_api.subscribe_new_blocks().await?);
        let (tip_slot, tip_slot_rx) = watch::channel(Slot::genesis());
        // A copy, as the loop needs the relays' adapter mutably at era changes.
        let mempool = relays.mempool_adapter().clone();
        let blend_adapter = BlendAdapter::<BlendService>::new(relays.blend_relay().clone());
        let mut proposing = pin!(
            propose_in_turn(
                slots_rx,
                tip_slot_rx,
                |slot| {
                    Self::build_block_for(
                        slot,
                        &cryptarchia_api,
                        &wallet_api,
                        &kms_api,
                        &mempool,
                        &era_schedule,
                        wallet_config.funding_pk,
                    )
                },
                |(block, reward)| {
                    Self::apply_and_publish_block_proposal(
                        block,
                        reward,
                        &wallet_api,
                        &chain_network_api,
                        &blend_adapter,
                    )
                },
            )
            .fuse()
        );

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
                        if era > current_era {
                            relays.mempool_adapter_mut().rotate_era(era).await;
                            current_era = era;
                        }
                        if slots.try_send(slot).is_err() {
                            metrics::consensus_proposals_create_failed("slots_queue_full");
                            error!(target: LOG_TARGET, "Too many slots waiting for the one underway: skipping {slot:?}");
                        }
                    }

                    Some(event) = new_blocks.next() => match event {
                        Ok(event) => {
                            tip_slot.send_replace(event.tip_slot);
                        }
                        // The next block brings the tip's slot up to date.
                        Err(lagged) => debug!(target: LOG_TARGET, "Missed new blocks: {lagged}"),
                    },

                    Some(transition_end) = era_transition_ends.next() => {
                        relays.mempool_adapter_mut().complete_era_transition(transition_end.era()).await;
                    }

                    Some(msg) = self.service_resources_handle.inbound_relay.next() => {
                        Self::handle_inbound_message(msg, &cryptarchia_api, &wallet_api, &kms_api, relays.time_relay(), &era_schedule);
                    }

                    () = &mut proposing => {}
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
    TimeBackend: lb_time_service::backends::TimeBackend,
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
    /// Builds the block of `slot` on the current tip, if a note of the node's
    /// wins it, with its reward note.
    #[expect(
        clippy::too_many_arguments,
        reason = "Everything a block of the node's is built from"
    )]
    async fn build_block_for(
        slot: Slot,
        cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
        wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
        kms_api: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
        mempool: &MempoolAdapter<Mempool::Item>,
        era_schedule: &EraScheduledConfig,
        reward_public_key: ZkPublicKey,
    ) -> Option<(Block<Mempool::Item>, Note)> {
        if Self::tip_has_passed(slot, cryptarchia_api).await {
            return None;
        }

        let Some(SlotContext {
            wallet_tip,
            epoch_state,
            eligible_aged,
            ..
        }) = fetch_slot_context(cryptarchia_api, wallet_api, slot).await
        else {
            error!(target: LOG_TARGET, "Failed to fetch epoch context for slot {slot:?}");
            return None;
        };
        tracing::debug!(
            target: LOG_TARGET,
            event = "leader_eligible_notes",
            path = "block_production",
            slot = ?slot,
            epoch = ?epoch_state.epoch,
            note_count = eligible_aged.len(),
            total_value = eligible_aged.iter().map(|note| note.note.note.value).sum::<u64>(),
            "Eligible notes for block production"
        );
        let tip_state = Self::tip_state(wallet_tip, cryptarchia_api).await?;

        // The witness of a win shows the note unspent at the tip, so the slot
        // is checked here, on the tip the block is built on. A winning note
        // whose witness fails, such as one spent since the epoch snapshot,
        // leaves the slot to the next winning note. The proof is produced once
        // the body is fixed, in `propose_block`.
        let latest = tip_state.latest_note_sets();
        let winning =
            find_winning_note(&eligible_aged, &epoch_state, latest, slot, kms_api).await?;

        Box::pin(Self::propose_block(
            wallet_tip,
            slot,
            winning,
            cryptarchia_api,
            wallet_api,
            kms_api,
            mempool,
            tip_state,
            era_schedule,
            reward_public_key,
        ))
        .await
        .map_err(|e| {
            let proof_stage = match e {
                Error::PrivateInputs(_) => Some("private_inputs"),
                Error::Proof(_) => Some("proof_generation"),
                Error::ProofTask(_) => Some("proof_task"),
                _ => None,
            };
            if let Some(stage) = proof_stage {
                metrics::consensus_proposals_create_failed(stage);
                error!(
                    target: LOG_TARGET,
                    diagnostic = BLEND_REACHABILITY,
                    event = "leadership_proof_failure",
                    epoch = u32::from(era_schedule.epoch_for_slot(slot)),
                    slot = u64::from(slot),
                    error = %e,
                    "Failed to build leadership proof"
                );
            } else {
                metrics::consensus_proposals_create_failed("propose_block");
                error!(target: LOG_TARGET, "{e}");
            }
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
        debug!(target: LOG_TARGET, ?slot, ?tip_slot, "Skipping a slot the tip has passed");
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
            winning,
            wallet_api,
            kms,
            reward_public_key
        )
    )]
    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Need all args"
    )]
    async fn propose_block(
        parent: HeaderId,
        slot: Slot,
        winning: WinningNote,
        cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
        wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
        kms: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
        mempool: &MempoolAdapter<Mempool::Item>,
        ledger_state: LedgerState,
        era_schedule: &EraScheduledConfig,
        reward_public_key: ZkPublicKey,
    ) -> Result<(Block<Mempool::Item>, Note), Error> {
        let view = mempool
            .get_mempool_view([0; 32].into())
            .await
            .map_err(Error::FetchBlockTransactions)?;

        let uncle_headers = cryptarchia_api
            .select_uncles(parent, slot)
            .await
            .unwrap_or_else(|err| {
                error!(target: LOG_TARGET, ?slot, %err, "failed to select uncles");
                // A proposal without uncles is still valid
                UncleHeaders::empty()
            });

        // The witness proves the note unspent in the note sets of the parent.
        let wallet_inputs = winning.wallet_inputs;

        // Selection decodes, applies and verifies every candidate, reading the
        // nullifier store as it goes, so it runs on the blocking pool.
        let uncle_slots = uncle_headers.slots();
        let schedule = era_schedule.clone();
        let selection = spawn_blocking("logos/chain/leader-selection-blocking", move || {
            // The proof is produced once the body is fixed, so the transactions
            // are selected against the header without it; the block is applied
            // with its proof by the chain network.
            let (ledger_state, _) = ledger_state.try_apply_proposal_header::<HeaderId>(
                slot,
                &uncle_slots,
                &schedule,
            )?;
            let header_state = ledger_state.clone();
            // The view decodes each transaction as it's read, so it's read
            // here. All candidates are collected up front, so the ones that
            // fail can be retried across multiple rounds.
            let candidates = Handle::current().block_on(view.collect());
            let TransactionSelection {
                ledger_state,
                selected_txs,
                invalid_tx_hashes,
            } = select_transactions(ledger_state, candidates, &schedule);
            let config = config_for_slot(&schedule, slot);
            let txs = txs_for_block(
                selected_txs,
                config.max_transactions,
                config.max_transactions_size,
            );
            // The PoL proves the reward note of the block, whose value is the
            // reward of the transactions the block ends up holding.
            let reward_value = block_leader_reward(header_state, txs.as_slice(), &schedule)?;
            Ok::<_, Error>((ledger_state, txs, invalid_tx_hashes, reward_value))
        });
        let (ledger_state, txs, invalid_tx_hashes, reward_value) =
            selection.await.map_err(Error::SelectionTask)??;

        if !invalid_tx_hashes.is_empty()
            && let Err(e) = mempool.remove_transactions(&invalid_tx_hashes).await
        {
            error!(target: LOG_TARGET, "Failed to remove invalid transactions from mempool: {e:?}");
        }

        // The wallet keeps the note only once the block is about to be
        // published, so a build given up on leaves nothing behind.
        let reward = wallet_api
            .new_leader_reward(reward_value, reward_public_key)
            .await?;
        let wallet_inputs = lb_pol::PolWalletInputsData {
            reward_public_key: *reward.public_key.as_fr(),
            reward_nonce: reward.nonce,
            ..wallet_inputs
        };

        let private = kms
            .build_private_inputs(
                winning.note.key_id.clone(),
                winning.public_inputs,
                wallet_inputs,
                reward.value,
            )
            .await?;
        let max_transactions_size = config_for_slot(era_schedule, slot).max_transactions_size;
        let block = spawn_blocking("logos/chain/leader-proof-blocking", move || {
            // The PoL is bound to the block ID, blocks carry no signature. The
            // block ID covers the entropy contribution, set before proving.
            let msg = block_msg(
                &parent,
                slot,
                &uncle_headers,
                txs.as_slice(),
                &reward.commitment(),
                reward.value,
                &private.entropy_contribution(),
            );
            assemble_block(
                parent,
                slot,
                uncle_headers,
                txs,
                max_transactions_size,
                private.with_msg(msg),
            )
        })
        .await??;
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

        Ok((block, reward))
    }

    /// Apply our own proposed block to the chain and publish it to the blend
    /// network, once the wallet keeps its `reward` note: the reward then
    /// can't be lost once the block is out.
    async fn apply_and_publish_block_proposal(
        block: Block<Mempool::Item>,
        reward: Note,
        wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
        chain_network_api: &ChainNetworkServiceApi<ChainNetwork>,
        blend_adapter: &BlendAdapter<BlendService>,
    ) {
        if let Err(e) = wallet_api.track_leader_reward(reward).await {
            metrics::consensus_proposals_create_failed("reward_note");
            error!(target: LOG_TARGET, "Failed to keep the reward note of our own proposed block {:?}: {e:?}", block.header().id());
            return;
        }

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

    fn handle_inbound_message(
        msg: LeaderMsg,
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        kms: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
        time_relay: &OutboundRelay<TimeServiceMessage>,
        era_schedule: &Arc<EraScheduledConfig>,
    ) {
        match msg {
            LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { sender } => {
                // Spawn a dedicated, off-main-loop task that scans winning
                // slots for this subscriber and streams them
                // in, with the bounded channel providing
                // backpressure.
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
}

/// Select transactions for a block, truncating `txs` at the first transaction
/// that trips the era's limits: `max_transactions` transactions, taking at most
/// `max_transactions_size` bytes.
fn txs_for_block<Tx, Txs>(
    txs: Txs,
    max_transactions: u16,
    max_transactions_size: u32,
) -> BlockTransactions<Tx>
where
    Tx: StorageSize,
    Txs: IntoIterator<Item = Tx>,
{
    let mut txs = txs.into_iter();
    let mut block_transactions_size: usize = 0;
    let mut selected_txs = Vec::new();

    while selected_txs.len() < usize::from(max_transactions) {
        let Some(tx) = txs.next() else {
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
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::{
        task::yield_now,
        time::{sleep, timeout},
    };

    use super::*;

    /// The slots, one after the other, with no more to come.
    fn slots(slots: impl IntoIterator<Item = u64>) -> mpsc::Receiver<Slot> {
        let (sender, receiver) = mpsc::channel(MAX_QUEUED_SLOTS);
        for slot in slots {
            assert!(sender.try_send(Slot::new(slot)).is_ok());
        }
        receiver
    }

    #[tokio::test]
    async fn slots_are_proposed_one_at_a_time_and_in_order() {
        let building = &AtomicUsize::new(0);
        let published = &Mutex::new(Vec::new());
        let (_tip, tip_slot) = watch::channel(Slot::genesis());

        propose_in_turn(
            slots([1, 2, 3]),
            tip_slot,
            async move |slot| {
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
    async fn a_build_the_tip_moves_past_gives_way_to_the_next_slot() {
        let published = &Mutex::new(Vec::new());
        let (stuck, next) = (Slot::new(1), Slot::new(3));
        let (tip, tip_slot) = watch::channel(Slot::genesis());

        let proposing = propose_in_turn(
            slots([1, 3]),
            tip_slot,
            move |slot| {
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
        let a_later_block_comes_in = async {
            yield_now().await;
            tip.send_replace(Slot::new(2));
        };
        timeout(Duration::from_secs(10), async {
            tokio::join!(proposing, a_later_block_comes_in)
        })
        .await
        .expect("The stuck build should give way once the tip has moved past its slot.");

        assert_eq!(*published.lock().unwrap(), [next]);
    }

    #[tokio::test]
    async fn a_block_of_the_same_slot_leaves_the_build_alone() {
        let published = &Mutex::new(Vec::new());
        let (tip, tip_slot) = watch::channel(Slot::genesis());
        let tip = &tip;

        propose_in_turn(
            slots([1]),
            tip_slot,
            async move |slot| {
                tip.send_replace(slot);
                sleep(Duration::from_millis(10)).await;
                Some(slot)
            },
            async move |slot| published.lock().unwrap().push(slot),
        )
        .await;

        assert_eq!(*published.lock().unwrap(), [Slot::new(1)]);
    }

    #[tokio::test]
    async fn the_tip_leaves_publishing_alone() {
        let published = &Mutex::new(Vec::new());
        let (tip, tip_slot) = watch::channel(Slot::genesis());
        let tip = &tip;

        propose_in_turn(
            slots([1]),
            tip_slot,
            async |slot| Some(slot),
            async move |slot| {
                tip.send_replace(Slot::new(2));
                sleep(Duration::from_millis(10)).await;
                published.lock().unwrap().push(slot);
            },
        )
        .await;

        assert_eq!(*published.lock().unwrap(), [Slot::new(1)]);
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

    #[test]
    fn block_tx_selection_respects_transaction_count_limit() {
        let txs = vec![TestTx { size: 1 }; usize::from(MAX_TRANSACTIONS) + 1];

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE);

        assert_eq!(selected.len(), usize::from(MAX_TRANSACTIONS));
    }

    #[test]
    fn block_tx_selection_respects_block_size_limit() {
        let txs = vec![
            TestTx { size: MAX_SIZE / 2 },
            TestTx { size: MAX_SIZE / 2 },
            TestTx { size: 1 },
        ];

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE);
        let selected_size: usize = selected.iter().map(StorageSize::storage_size).sum();

        assert_eq!(selected.len(), 2);
        assert_eq!(selected_size, MAX_SIZE);
    }

    #[test]
    fn block_tx_selection_stops_at_first_transaction_that_does_not_fit() {
        // The middle transaction does not fit alongside the first, so selection
        // must stop there and must not pull the third (which would fit on its
        // own) ahead of it — doing so could drop a dependency of the third.
        let txs = vec![
            TestTx { size: 10 },
            TestTx { size: MAX_SIZE },
            TestTx { size: 10 },
        ];

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected.as_slice()[0].storage_size(), 10);
    }

    #[test]
    fn block_tx_selection_stops_at_leading_oversized_transaction() {
        // A transaction larger than the whole block can never fit. Selection
        // stops at it rather than skipping past to later transactions, which
        // may depend on it. (In practice such transactions are filtered
        // out before reaching here, but the prefix invariant must hold
        // regardless.)
        let txs = vec![TestTx { size: MAX_SIZE + 1 }, TestTx { size: 1 }];

        let selected = txs_for_block(txs, MAX_TRANSACTIONS, MAX_TRANSACTIONS_SIZE);

        assert!(selected.is_empty());
    }
}
