pub mod api;
mod blend;
mod kms;
mod leadership;
mod mempool;
mod metrics;
mod relays;
mod wallet;

use core::fmt::Debug;
use std::{fmt::Display, iter, pin::Pin, time::Duration};

use futures::{Stream, StreamExt as _, stream};
use lb_chain_network_service::api::{ChainNetworkServiceApi, ChainNetworkServiceData};
use lb_chain_service::{
    Epoch,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    block::{Block, Error as BlockError, MAX_BLOCK_SIZE, MAX_BLOCK_TRANSACTIONS},
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, SignedMantleTx, StorageSize, Transaction, TxHash, TxSelect,
        gas::MainnetGasConstants, ops::leader_claim::LeaderClaimOp,
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate},
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_service::{api::KmsServiceApi, keys::Ed25519Key};
use lb_ledger::LedgerState;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::{SlotTick, TimeService, TimeServiceMessage};
use lb_tx_service::{
    TxMempoolService,
    backend::{MemPool, RecoverableMempool},
    network::NetworkAdapter as MempoolNetworkAdapter,
    storage::MempoolStorageAdapter,
};
use lb_wallet_service::api::{WalletApi, WalletApiError};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, relay::OutboundRelay},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{Level, error, info, instrument, span, trace};
use tracing_futures::Instrument as _;

pub use crate::wallet::LeaderWalletConfig;
use crate::{
    blend::BlendAdapter,
    kms::PreloadKmsService,
    leadership::{SlotContext, build_proof_for, fetch_slot_context, search_for_winning_slots},
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
    relays::CryptarchiaConsensusRelays,
    wallet::{LeaderWalletError, fund_and_sign_leader_claim_tx},
};

/// The per-subscriber stream of per-epoch winning slots. Each item
/// carries a single epoch and that epoch's stream of winning slots.
///
/// `Send` but not `Sync`: each item carries a [`WinningPolSlotStream`] of
/// `Send`-only per-slot futures (see [`WinningSlotFuture`]), so the handoff is
/// not `Sync`.
pub type WinningPolEpochSlotsStream =
    Pin<Box<dyn Stream<Item = WinningPolEpochSlots> + Send + Unpin>>;

pub struct WinningPolEpochSlots {
    pub epoch: Epoch,
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

pub(crate) const LOG_TARGET: &str = "chain_leader::service";

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
    #[error("Leader wallet error: {0}")]
    LeaderWallet(#[from] LeaderWalletError),
    #[error("Mempool error: {0}")]
    Mempool(#[source] DynError),
    #[error("Chain service error: {0}")]
    ChainService(#[from] lb_chain_service::api::ApiError),
    #[error("No claimable voucher found")]
    NoClaimableVoucher,
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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LeaderSettings<Ts, BlendBroadcastSettings> {
    #[serde(default)]
    pub transaction_selector_settings: Ts,
    pub config: lb_ledger::Config,
    pub blend_broadcast_settings: BlendBroadcastSettings,
    pub wallet_config: LeaderWalletConfig,
}

#[expect(clippy::allow_attributes_without_reason)]
pub struct CryptarchiaLeader<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    TxS,
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
    Mempool::Item: AuthenticatedMantleTx,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TxS: TxSelect<Tx = Mempool::Item>,
    TxS::Settings: Send,
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
    TxS,
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
        TxS,
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
    Mempool::Item: AuthenticatedMantleTx + Clone + Eq + Debug,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TxS: TxSelect<Tx = Mempool::Item>,
    TxS::Settings: Send,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData,
    ChainNetwork: ChainNetworkServiceData,
    Wallet: lb_wallet_service::api::WalletServiceData,
{
    type Settings = LeaderSettings<TxS::Settings, BlendService::BroadcastSettings>;
    type State = overwatch::services::state::NoState<Self::Settings>;
    type StateOperator = overwatch::services::state::NoOperator<Self::State>;
    type Message = LeaderMsg;
}

#[async_trait::async_trait]
impl<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    TxS,
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
        TxS,
        TimeBackend,
        CryptarchiaService,
        ChainNetwork,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: ServiceData<
            Message = lb_blend_service::message::ProxyServiceMessage<
                lb_blend_service::message::ServiceMessage<
                    BlendService::BroadcastSettings,
                    BlendService::NodeId,
                >,
            >,
        > + lb_blend_service::ServiceComponents<NodeId: Send + Sync>
        + Send
        + 'static,
    BlendService::BroadcastSettings: Clone + Send + Sync,
    Mempool: MemPool<Item = SignedMantleTx>
        + RecoverableMempool<BlockId = HeaderId, Key = TxHash>
        + Send
        + Sync
        + 'static,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Item: Transaction<Hash = Mempool::Key>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Mempool::Item: AuthenticatedMantleTx,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TxS: TxSelect<Tx = Mempool::Item> + Clone + Send + Sync + 'static,
    TxS::Settings: Send + Sync + 'static,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item> + 'static,
    ChainNetwork: ChainNetworkServiceData<Tx = Mempool::Item>,
    Wallet: lb_wallet_service::api::WalletServiceData + 'static,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<BlendService>
        + AsServiceId<
            TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>,
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
        let relays = CryptarchiaConsensusRelays::from_service_resources_handle::<
            Self,
            TimeBackend,
            CryptarchiaService,
        >(&self.service_resources_handle)
        .await;

        // Create the API wrapper for chain service communication
        let cryptarchia_api = CryptarchiaServiceApi::<CryptarchiaService, RuntimeServiceId>::new(
            self.service_resources_handle
                .overwatch_handle
                .relay::<CryptarchiaService>()
                .await
                .expect("Failed to estabilish connection with Cryptarchia"),
        );

        let chain_network_api = ChainNetworkServiceApi::<ChainNetwork, RuntimeServiceId>::new(
            self.service_resources_handle
                .overwatch_handle
                .relay::<ChainNetwork>()
                .await
                .expect("Failed to estabilish connection with ChainNetwork"),
        );

        let LeaderSettings {
            config: ledger_config,
            transaction_selector_settings,
            blend_broadcast_settings,
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

        let tx_selector = TxS::new(transaction_selector_settings);

        let blend_adapter = BlendAdapter::<BlendService>::new(
            relays.blend_relay().clone(),
            blend_broadcast_settings.clone(),
        );

        // Wait for other services to become ready, with timeout.
        // (except Chain and ChainLeader)
        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_mins(1)),
            BlendService,
            TxMempoolService<_, _, _, _>,
            TimeService<_, _>,
            Wallet,
            PreloadKmsService<_>
        )
        .await?;
        // Wait for Chain and ChainLeader services to become ready, without timeout
        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            None,
            CryptarchiaService, // becomes ready after recoverying blocks
            ChainNetwork        // becomes ready after IBD
        )
        .await?;

        let mut slot_timer = {
            let (sender, receiver) = oneshot::channel();
            relays
                .time_relay()
                .send(TimeServiceMessage::Subscribe { sender })
                .await
                .expect("Request time subscription to time service should succeed");
            receiver.await?
        };

        // Wait until the chain becomes Online mode.
        // We should not propose blocks while the chain is in Bootstrapping mode.
        info!("Waiting for chain to become Online mode");
        cryptarchia_api
            .wait_until_chain_becomes_online()
            .await
            .expect("Waiting for chain to be online should succeed");
        info!("Chain is now Online. Starting block proposals.");

        self.service_resources_handle.status_updater.notify_ready();
        info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        let async_loop = async {
            loop {
                tokio::select! {
                    Some(SlotTick { slot, epoch }) = slot_timer.next() => {
                        trace!(target: LOG_TARGET, "Received SlotTick for slot {}, ep {}", u64::from(slot), u32::from(epoch));
                        let Some(SlotContext { tip, epoch_state, eligible_aged }) =
                            fetch_slot_context(&cryptarchia_api, &wallet_api, &ledger_config, slot).await
                        else {
                            error!(target: LOG_TARGET, "Failed to fetch epoch context for slot {slot:?}");
                            continue;
                        };

                        // The block-proposal proof must prove the winning note is still
                        // unspent, so it needs the latest tip ledger state (fetched per slot).
                        let tip_state = match cryptarchia_api.get_ledger_state(tip).await {
                            Ok(Some(state)) => state,
                            Ok(None) => {
                                error!(target: LOG_TARGET, "Ledger state not found for tip {tip:?}");
                                continue;
                            }
                            Err(e) => {
                                error!(target: LOG_TARGET, "Failed to get ledger state for tip: {e}");
                                continue;
                            }
                        };

                        let latest_tree = tip_state.latest_utxos();

                       if let Some((proof, signing_key)) = build_proof_for(&eligible_aged, latest_tree, &epoch_state, slot, &wallet_api, &kms_api).await {
                            // TODO: spawn as a separate task?
                            match Self::propose_block(
                                tip,
                                slot,
                                proof,
                                &signing_key,
                                tx_selector.clone(),
                                &relays,
                                tip_state,
                                &ledger_config,
                            )
                            .await
                            {
                                Ok(block) => {
                                    Self::apply_and_publish_block_proposal(block, &chain_network_api, &blend_adapter).await;
                                }
                                Err(e) => {
                                    metrics::consensus_proposals_create_failed();
                                    error!(target: LOG_TARGET, "{e}");
                                }
                            }
                        }
                    }

                    Some(msg) = self.service_resources_handle.inbound_relay.next() => {
                        Self::handle_inbound_message(msg, &cryptarchia_api, &wallet_api, &kms_api, relays.time_relay(), &ledger_config, &wallet_config, relays.mempool_adapter()).await;
                    }
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
        async_loop.instrument(span!(Level::TRACE, SERVICE_ID)).await;

        Ok(())
    }
}

impl<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    TxS,
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
        TxS,
        TimeBackend,
        CryptarchiaService,
        ChainNetwork,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: ServiceData<
            Message = lb_blend_service::message::ProxyServiceMessage<
                lb_blend_service::message::ServiceMessage<
                    BlendService::BroadcastSettings,
                    BlendService::NodeId,
                >,
            >,
        > + lb_blend_service::ServiceComponents<NodeId: Send + Sync>
        + Send
        + 'static,
    BlendService::BroadcastSettings: Clone + Send + Sync,
    Mempool: MemPool<Item = SignedMantleTx>
        + RecoverableMempool<BlockId = HeaderId, Key = TxHash>
        + Send
        + Sync
        + 'static,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Item: AuthenticatedMantleTx<Hash = Mempool::Key>
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
    TxS: TxSelect<Tx = Mempool::Item> + Clone + Send + Sync + 'static,
    TxS::Settings: Send + Sync + 'static,
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
    #[expect(clippy::allow_attributes_without_reason)]
    #[expect(
        clippy::too_many_arguments,
        reason = "All arguments are required for proposing a block"
    )]
    #[instrument(
        level = "debug",
        skip(tx_selector, relays, ledger_state, ledger_config, proof, signing_key)
    )]
    async fn propose_block(
        parent: HeaderId,
        slot: Slot,
        proof: Groth16LeaderProof,
        signing_key: &Ed25519Key,
        tx_selector: TxS,
        relays: &CryptarchiaConsensusRelays<
            BlendService,
            Mempool,
            MempoolNetAdapter,
            RuntimeServiceId,
        >,
        mut ledger_state: LedgerState,
        ledger_config: &lb_ledger::Config,
    ) -> Result<Block<Mempool::Item>, Error> {
        let txs_stream = relays
            .mempool_adapter()
            .get_mempool_view([0; 32].into())
            .await
            .map_err(Error::FetchBlockTransactions)?;

        let tx_stream: Pin<Box<_>> = Box::pin(txs_stream);

        ledger_state = ledger_state
            .clone()
            .try_apply_header::<Groth16LeaderProof, HeaderId>(slot, &proof, ledger_config)?;

        // Collect all candidate transactions up front so the ones that fail can
        // be retried across multiple rounds.
        let mut pending: Vec<_> = tx_stream.collect().await;

        let mut valid_txs = Vec::new();

        // A transaction may only become valid once another transaction it depends
        // on has already been applied. Repeatedly attempt to apply the pending
        // transactions, retrying the full set of failures each round, while a
        // round keeps adding new transactions to the block.
        let mut applied_any = true;
        while applied_any {
            applied_any = false;
            let mut still_pending = Vec::with_capacity(pending.len());

            for tx in pending {
                match ledger_state
                    .clone()
                    .try_apply_contents::<HeaderId, MainnetGasConstants>(
                        ledger_config,
                        iter::once(tx.clone()),
                    ) {
                    Ok((new_state, _events)) => {
                        ledger_state = new_state;
                        valid_txs.push(tx);
                        applied_any = true;
                    }
                    Err(err) => {
                        tracing::trace!(
                            "tx {:?} not (yet) applicable during block assembly: {:?}",
                            tx.hash(),
                            err
                        );
                        still_pending.push(tx);
                    }
                }
            }

            pending = still_pending;
        }

        // Transactions that never became applicable are genuinely invalid against
        // this block's ledger state and can be evicted from the mempool.
        let invalid_tx_hashes: Vec<_> = pending.iter().map(Transaction::hash).collect();

        if !invalid_tx_hashes.is_empty()
            && let Err(e) = relays
                .mempool_adapter()
                .remove_transactions(&invalid_tx_hashes)
                .await
        {
            error!("Failed to remove invalid transactions from mempool: {e:?}");
        }

        let valid_tx_stream = stream::iter(valid_txs);
        let txs = txs_for_block(tx_selector.select_tx_from(valid_tx_stream)).await;

        let block = Block::create(parent, slot, proof, txs, signing_key)?;

        info!(
            "proposed block with id {:?} containing {} transactions ({} removed)",
            block.header().id(),
            block.transactions().len(),
            invalid_tx_hashes.len()
        );

        Ok(block)
    }

    /// Apply our own proposed block to the chain and publish it to the blend
    /// network.
    async fn apply_and_publish_block_proposal(
        block: Block<Mempool::Item>,
        chain_network_api: &ChainNetworkServiceApi<ChainNetwork, RuntimeServiceId>,
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
        reason = "All handles are required to serve the winning-slot subscription off the main loop."
    )]
    async fn handle_inbound_message(
        msg: LeaderMsg,
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        kms: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
        time_relay: &OutboundRelay<TimeServiceMessage>,
        ledger_config: &lb_ledger::Config,
        config: &LeaderWalletConfig,
        mempool: &MempoolAdapter<Mempool::Item>,
    ) {
        match msg {
            LeaderMsg::Claim { sender } => {
                Self::handle_claim_message(cryptarchia, wallet, config, mempool, sender).await;
            }
            LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { sender } => {
                // Spawn a dedicated, off-main-loop task that scans winning slots
                // for this subscriber and streams them in, with the bounded
                // channel providing backpressure.
                let (epoch_handoff_sender, epoch_handoff_receiver) =
                    mpsc::channel(WINNING_POL_EPOCH_HANDOFF_BUFFER_SIZE);
                tokio::spawn(search_for_winning_slots(
                    (*cryptarchia).clone(),
                    (*wallet).clone(),
                    (*kms).clone(),
                    (*time_relay).clone(),
                    (*ledger_config).clone(),
                    epoch_handoff_sender,
                ));
                let stream: WinningPolEpochSlotsStream =
                    Box::pin(ReceiverStream::new(epoch_handoff_receiver));
                if sender.send(stream).is_err() {
                    error!(target: LOG_TARGET, "Could not send winning PoL epoch slots stream to subscriber.");
                }
            }
        }
    }

    async fn handle_claim_message(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        config: &LeaderWalletConfig,
        mempool: &MempoolAdapter<Mempool::Item>,
        resp_tx: oneshot::Sender<Result<TxHash, Error>>,
    ) {
        let result = Self::build_and_submit_claim_tx(cryptarchia, wallet, mempool, config).await;
        if resp_tx.send(result).is_err() {
            error!("Failed to send claim response");
        }
    }

    async fn build_and_submit_claim_tx(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        mempool: &MempoolAdapter<Mempool::Item>,
        config: &LeaderWalletConfig,
    ) -> Result<TxHash, Error> {
        let (tip, ledger_state) = Self::get_tip_ledger_state(cryptarchia).await?;

        let voucher_nullifier = wallet
            .get_claimable_voucher(Some(tip))
            .await?
            .response
            .ok_or(Error::NoClaimableVoucher)?
            .nullifier;
        let pks = wallet.get_known_addresses().await?;

        // TODO: let the user chose where to receive the rewards
        let reward_amount = ledger_state.mantle_ledger().leader_reward_amount();
        let signed_tx = fund_and_sign_leader_claim_tx(
            LeaderClaimOp {
                rewards_root: ledger_state.mantle_ledger().vouchers_snapshot_root(),
                voucher_nullifier,
                pk: pks[0],
            },
            reward_amount,
            tip,
            wallet,
            config,
        )
        .await?;
        let tx_hash = signed_tx.hash();

        mempool.post_tx(signed_tx).await.map_err(Error::Mempool)?;
        Ok(tx_hash)
    }

    async fn get_tip_ledger_state(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
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
/// transaction that trips the block size or count limits.
async fn txs_for_block<Tx, S>(mut txs: S) -> Vec<Tx>
where
    Tx: StorageSize,
    S: Stream<Item = Tx> + Unpin,
{
    let mut block_size: usize = 0;
    let mut selected_txs = Vec::new();

    while selected_txs.len() < MAX_BLOCK_TRANSACTIONS {
        let Some(tx) = txs.next().await else {
            break;
        };

        let tx_size = tx.storage_size();
        let Some(next_block_size) = block_size.checked_add(tx_size) else {
            break;
        };

        if next_block_size > MAX_BLOCK_SIZE {
            break;
        }

        block_size = next_block_size;
        selected_txs.push(tx);
    }

    selected_txs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestTx {
        size: usize,
    }

    impl StorageSize for TestTx {
        fn storage_size(&self) -> usize {
            self.size
        }
    }

    #[tokio::test]
    async fn block_tx_selection_respects_transaction_count_limit() {
        let txs = stream::iter(vec![TestTx { size: 1 }; MAX_BLOCK_TRANSACTIONS + 1]);

        let selected = txs_for_block(txs).await;

        assert_eq!(selected.len(), MAX_BLOCK_TRANSACTIONS);
    }

    #[tokio::test]
    async fn block_tx_selection_respects_block_size_limit() {
        let txs = stream::iter(vec![
            TestTx {
                size: MAX_BLOCK_SIZE / 2,
            },
            TestTx {
                size: MAX_BLOCK_SIZE / 2,
            },
            TestTx { size: 1 },
        ]);

        let selected = txs_for_block(txs).await;
        let selected_size: usize = selected.iter().map(StorageSize::storage_size).sum();

        assert_eq!(selected.len(), 2);
        assert_eq!(selected_size, MAX_BLOCK_SIZE);
    }

    #[tokio::test]
    async fn block_tx_selection_stops_at_first_transaction_that_does_not_fit() {
        // The middle transaction does not fit alongside the first, so selection
        // must stop there and must not pull the third (which would fit on its
        // own) ahead of it — doing so could drop a dependency of the third.
        let txs = stream::iter(vec![
            TestTx { size: 10 },
            TestTx {
                size: MAX_BLOCK_SIZE,
            },
            TestTx { size: 10 },
        ]);

        let selected = txs_for_block(txs).await;

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].storage_size(), 10);
    }

    #[tokio::test]
    async fn block_tx_selection_stops_at_leading_oversized_transaction() {
        // A transaction larger than the whole block can never fit. Selection
        // stops at it rather than skipping past to later transactions, which may
        // depend on it. (In practice such transactions are filtered out before
        // reaching here, but the prefix invariant must hold regardless.)
        let txs = stream::iter(vec![
            TestTx {
                size: MAX_BLOCK_SIZE + 1,
            },
            TestTx { size: 1 },
        ]);

        let selected = txs_for_block(txs).await;

        assert!(selected.is_empty());
    }
}
