mod blend;
mod leadership;
mod mempool;
mod relays;

use core::fmt::Debug;
use std::{collections::BTreeSet, fmt::Display, iter, pin::Pin, time::Duration};

use chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use cryptarchia_engine::{Epoch, Slot};
use futures::{StreamExt as _, future, stream};
use key_management_system_keys::keys::Ed25519Key;
pub use leadership::LeaderConfig;
use nomos_core::{
    block::{Block, Error as BlockError, MAX_TRANSACTIONS},
    da,
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, Op, Transaction, TxHash, TxSelect, gas::MainnetGasConstants,
        ops::leader_claim::VoucherCm,
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate},
};
use nomos_da_sampling::{
    DaSamplingService, DaSamplingServiceMsg, backend::DaSamplingServiceBackend,
    mempool::DaMempoolAdapter,
};
use nomos_time::{SlotTick, TimeService, TimeServiceMessage};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, relay::OutboundRelay},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use services_utils::wait_until_services_are_ready;
use thiserror::Error;
use tokio::sync::{oneshot, watch};
use tracing::{Level, error, info, instrument, span};
use tracing_futures::Instrument as _;
use tx_service::{
    TxMempoolService,
    backend::{MemPool, RecoverableMempool},
    network::NetworkAdapter as MempoolNetworkAdapter,
    storage::MempoolStorageAdapter,
};

use crate::{
    blend::BlendAdapter,
    leadership::{Leader, WinningPoLSlotNotifier},
    mempool::MempoolAdapter as _,
    relays::CryptarchiaConsensusRelays,
};

type SamplingRelay<BlobId> = OutboundRelay<DaSamplingServiceMsg<BlobId>>;
pub(crate) type WinningPolInfo = (LeaderPrivate, Epoch);

const LEADER_ID: &str = "Leader";

pub(crate) const LOG_TARGET: &str = "cryptarchia::leader";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Ledger error: {0}")]
    Ledger(#[from] nomos_ledger::LedgerError<HeaderId>),
    #[error("Consensus error: {0}")]
    Consensus(#[from] cryptarchia_engine::Error<HeaderId>),
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Could not fetch block transactions: {0}")]
    FetchBlockTransactions(#[source] DynError),
    #[error("Failed to create valid block during proposal: {0}")]
    BlockCreation(#[from] BlockError),
}

#[derive(Debug)]
pub enum LeaderMsg {
    /// Request a new receiver that yields PoL-winning slot information.
    ///
    /// The stream will yield items in one of the following cases:
    /// * a new epoch starts -> immediately the first winning slot of the new
    ///   epoch, if any
    /// * this service is started mid-epoch -> immediately the first winning
    ///   slot of the ongoing epoch (the slot can also be in the past compared
    ///   to the current slot as returned by the time service), if any
    /// * a new winning slot (other than the very first one in the ongoing
    ///   epoch) is identified when proposing blocks
    /// * a new consumer subscribes -> the latest value that was sent to all the
    ///   other consumers, if any
    WinningPolEpochSlotStreamSubscribe {
        sender: oneshot::Sender<watch::Receiver<Option<WinningPolInfo>>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LeaderSettings<Ts, BlendBroadcastSettings> {
    #[serde(default)]
    pub transaction_selector_settings: Ts,
    pub config: nomos_ledger::Config,
    pub leader_config: LeaderConfig,
    pub blend_broadcast_settings: BlendBroadcastSettings,
}

#[expect(clippy::allow_attributes_without_reason)]
pub struct CryptarchiaLeader<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    TxS,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
    TimeBackend,
    CryptarchiaService,
    Wallet,
    RuntimeServiceId,
> where
    BlendService: nomos_blend_service::ServiceComponents,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Settings: Clone,
    Mempool::Item: Clone + Eq + Debug + 'static,
    Mempool::Item: AuthenticatedMantleTx,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    MempoolDaAdapter: DaMempoolAdapter + Send + Sync + 'static,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TxS: TxSelect<Tx = Mempool::Item>,
    TxS::Settings: Send,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
    SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData,
    Wallet: nomos_wallet::api::WalletServiceData,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    winning_pol_epoch_slots_sender: watch::Sender<Option<WinningPolInfo>>,
}

impl<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    TxS,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
    TimeBackend,
    CryptarchiaService,
    Wallet,
    RuntimeServiceId,
> ServiceData
    for CryptarchiaLeader<
        BlendService,
        Mempool,
        MempoolNetAdapter,
        MempoolDaAdapter,
        TxS,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
        TimeBackend,
        CryptarchiaService,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: nomos_blend_service::ServiceComponents,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::Settings: Clone,
    Mempool::Item: AuthenticatedMantleTx + Clone + Eq + Debug,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    MempoolDaAdapter: DaMempoolAdapter + Send + Sync + 'static,
    TxS: TxSelect<Tx = Mempool::Item>,
    TxS::Settings: Send,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
    SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData,
    Wallet: nomos_wallet::api::WalletServiceData,
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
    MempoolDaAdapter,
    TxS,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
    TimeBackend,
    CryptarchiaService,
    Wallet,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for CryptarchiaLeader<
        BlendService,
        Mempool,
        MempoolNetAdapter,
        MempoolDaAdapter,
        TxS,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
        TimeBackend,
        CryptarchiaService,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: ServiceData<
            Message = nomos_blend_service::message::ServiceMessage<BlendService::BroadcastSettings>,
        > + nomos_blend_service::ServiceComponents
        + Send
        + Sync
        + 'static,
    BlendService::BroadcastSettings: Clone + Send + Sync,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync + 'static,
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
    MempoolDaAdapter: DaMempoolAdapter + Send + Sync + 'static,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    TxS: TxSelect<Tx = Mempool::Item> + Clone + Send + Sync + 'static,
    TxS::Settings: Send + Sync + 'static,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send + 'static,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + Send + 'static,
    SamplingNetworkAdapter:
        nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId> + Send + Sync + 'static,
    SamplingStorage:
        nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item>,
    Wallet: nomos_wallet::api::WalletServiceData,
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
        + AsServiceId<
            DaSamplingService<
                SamplingBackend,
                SamplingNetworkAdapter,
                SamplingStorage,
                MempoolDaAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<CryptarchiaService>
        + AsServiceId<Wallet>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let winning_pol_epoch_slots_sender = watch::Sender::new(None);

        Ok(Self {
            service_resources_handle,
            winning_pol_epoch_slots_sender,
        })
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    async fn run(mut self) -> Result<(), DynError> {
        let relays = CryptarchiaConsensusRelays::from_service_resources_handle::<
            Self,
            SamplingNetworkAdapter,
            SamplingStorage,
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

        let LeaderSettings {
            config: ledger_config,
            transaction_selector_settings,
            leader_config,
            blend_broadcast_settings,
        } = self
            .service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        // TODO: check active slot coeff is exactly 1/30

        let leader = Leader::new(leader_config.sk, ledger_config.clone());
        let mut winning_pol_slot_notifier =
            WinningPoLSlotNotifier::new(&leader, &self.winning_pol_epoch_slots_sender);

        let wallet_api = nomos_wallet::api::WalletApi::<Wallet, RuntimeServiceId>::new(
            self.service_resources_handle
                .overwatch_handle
                .relay::<Wallet>()
                .await?,
        );

        let tx_selector = TxS::new(transaction_selector_settings);

        let blend_adapter = BlendAdapter::<BlendService>::new(
            relays.blend_relay().clone(),
            blend_broadcast_settings.clone(),
        );

        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            BlendService,
            TxMempoolService<_, _, _, _>,
            DaSamplingService<_, _, _, _, _>,
            TimeService<_, _>,
            CryptarchiaService,
            Wallet
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

        self.service_resources_handle.status_updater.notify_ready();
        info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        let async_loop = async {
            loop {
                tokio::select! {
                    Some(SlotTick { slot, epoch }) = slot_timer.next() => {
                        info!("Received SlotTick for slot {}, ep {}", u64::from(slot), u32::from(epoch));
                        let chain_info = match cryptarchia_api.info().await {
                            Ok(info) => info,
                            Err(e) => {
                                error!("Failed to get chain info: {:?}", e);
                                continue;
                            }
                        };
                        let parent = chain_info.tip;

                        let tip_state = match cryptarchia_api.get_ledger_state(parent).await {
                            Ok(Some(state)) => state,
                            Ok(None) => {
                                error!("No ledger state found for tip {:?}", parent);
                                continue;
                            }
                            Err(e) => {
                                error!("Failed to get ledger state: {:?}", e);
                                continue;
                            }
                        };

                        let latest_tree = tip_state.latest_utxos();

                        let epoch_state = match cryptarchia_api.get_epoch_state(slot).await {
                            Ok(Some(state)) => state,
                            Ok(None) => {
                                error!("trying to propose a block for slot {} but epoch state is not available", u64::from(slot));
                                continue;
                            }
                            Err(e) => {
                                error!("Failed to get epoch state: {:?}", e);
                                continue;
                            }
                        };

                        let eligible_utxos = match wallet_api.get_leader_aged_notes(parent).await {
                            Ok(utxos) => utxos,
                            Err(e) => {
                                error!("Failed to fetch leader aged notes from wallet: {:?}", e);
                                continue;
                            }
                        };

                        // If it's a new epoch or the service just started, pre-compute the first winning slot and notify consumers.
                        winning_pol_slot_notifier.process_epoch(&eligible_utxos, &epoch_state);

                        if let Some(proof) = leader.build_proof_for(&eligible_utxos, latest_tree, &epoch_state, slot, &winning_pol_slot_notifier).await {
                            // TODO: spawn as a separate task?
                            match Self::propose_block(
                                parent,
                                slot,
                                proof,
                                tx_selector.clone(),
                                &relays,
                                tip_state,
                                &ledger_config,
                            )
                            .await
                            {
                                Ok(block) => {
                                    // Process our own block first to ensure it's valid
                                    match cryptarchia_api.apply_block(block.clone()).await {
                                        Ok(()) => {
                                            // Block successfully processed, now publish it to the network
                                            let proposal = block.to_proposal();
                                            blend_adapter.publish_proposal(proposal).await;
                                        }
                                        Err(e) => {
                                            error!(target: LOG_TARGET, "Error processing local block: {:?}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!(target: LOG_TARGET, "{e}");
                                }
                            }
                        }
                    }

                    Some(msg) = self.service_resources_handle.inbound_relay.next() => {
                        handle_inbound_message(msg, &self.winning_pol_epoch_slots_sender);
                    }
                }
            }
        };

        // It sucks to use `LEADER_ID` when we have `<RuntimeServiceId as
        // AsServiceId<Self>>::SERVICE_ID`.
        // Somehow it just does not let us use it.
        //
        // Hypothesis:
        // 1. Probably related to too many generics.
        // 2. It seems `span` requires a `const` string literal.
        async_loop.instrument(span!(Level::TRACE, LEADER_ID)).await;

        Ok(())
    }
}

impl<
    BlendService,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    TxS,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
    TimeBackend,
    CryptarchiaService,
    Wallet,
    RuntimeServiceId,
>
    CryptarchiaLeader<
        BlendService,
        Mempool,
        MempoolNetAdapter,
        MempoolDaAdapter,
        TxS,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
        TimeBackend,
        CryptarchiaService,
        Wallet,
        RuntimeServiceId,
    >
where
    BlendService: ServiceData<
            Message = nomos_blend_service::message::ServiceMessage<BlendService::BroadcastSettings>,
        > + nomos_blend_service::ServiceComponents
        + Send
        + Sync
        + 'static,
    BlendService::BroadcastSettings: Send + Sync,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync + 'static,
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
    Mempool::Item: AuthenticatedMantleTx,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    MempoolDaAdapter: DaMempoolAdapter + Send + Sync + 'static,
    <MempoolNetAdapter as MempoolNetworkAdapter<RuntimeServiceId>>::Settings: Send + Sync,
    <Mempool as MemPool>::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    TxS: TxSelect<Tx = Mempool::Item> + Clone + Send + Sync + 'static,
    TxS::Settings: Send + Sync + 'static,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
    SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
    CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item>,
    Wallet: nomos_wallet::api::WalletServiceData,
    RuntimeServiceId: Sync + Send + 'static,
{
    #[expect(clippy::allow_attributes_without_reason)]
    #[instrument(
        level = "debug",
        skip(tx_selector, relays, ledger_state, ledger_config)
    )]
    async fn propose_block(
        parent: HeaderId,
        slot: Slot,
        proof: Groth16LeaderProof,
        tx_selector: TxS,
        relays: &CryptarchiaConsensusRelays<
            BlendService,
            Mempool,
            MempoolNetAdapter,
            MempoolDaAdapter,
            SamplingBackend,
            RuntimeServiceId,
        >,
        mut ledger_state: nomos_ledger::LedgerState,
        ledger_config: &nomos_ledger::Config,
    ) -> Result<Block<Mempool::Item>, Error> {
        let txs = relays.mempool_adapter().get_mempool_view([0; 32].into());
        let sampling_relay = relays.sampling_relay().clone();
        let blobs_ids = get_sampled_blobs(sampling_relay);

        let (txs_stream, blobs) =
            futures::try_join!(txs, blobs_ids).map_err(Error::FetchBlockTransactions)?;

        let filtered_stream = txs_stream.filter(move |tx| {
            let is_valid = tx.mantle_tx().ops.iter().all(|op| match op {
                Op::ChannelBlob(op) => blobs.contains(&op.blob),
                _ => true,
            });

            future::ready(is_valid)
        });

        let mut tx_stream: Pin<Box<_>> = Box::pin(filtered_stream);

        ledger_state = ledger_state
            .clone()
            .try_apply_header::<Groth16LeaderProof, HeaderId>(
                slot,
                &proof,
                VoucherCm::default(),
                ledger_config,
            )?;

        let mut valid_txs = Vec::new();
        let mut invalid_tx_hashes = Vec::new();

        while let Some(tx) = tx_stream.next().await {
            let tx_hash = tx.hash();
            match ledger_state
                .clone()
                .try_apply_contents::<HeaderId, MainnetGasConstants>(
                    ledger_config,
                    iter::once(tx.clone()),
                ) {
                Ok(new_state) => {
                    ledger_state = new_state;
                    valid_txs.push(tx);
                }
                Err(err) => {
                    tracing::debug!(
                        "failed to apply tx {:?} during block assembly: {:?}",
                        tx_hash,
                        err
                    );
                    invalid_tx_hashes.push(tx_hash);
                }
            }
        }

        if !invalid_tx_hashes.is_empty()
            && let Err(e) = relays
                .mempool_adapter()
                .remove_transactions(&invalid_tx_hashes)
                .await
        {
            error!("Failed to remove invalid transactions from mempool: {e:?}");
        }

        let valid_tx_stream = stream::iter(valid_txs);
        let selected_txs_stream = tx_selector.select_tx_from(valid_tx_stream);
        let txs: Vec<_> = selected_txs_stream.take(MAX_TRANSACTIONS).collect().await;

        // TODO: use PoL signing key
        let dummy_signing_key = Ed25519Key::from_bytes(&[0u8; 32]);

        let block = Block::create(parent, slot, proof, txs, &dummy_signing_key)?;

        info!(
            "proposed block with id {:?} containing {} transactions ({} removed)",
            block.header().id(),
            block.transactions().len(),
            invalid_tx_hashes.len()
        );

        Ok(block)
    }
}

async fn get_sampled_blobs<BlobId>(
    sampling_relay: SamplingRelay<BlobId>,
) -> Result<BTreeSet<BlobId>, DynError>
where
    BlobId: Send,
{
    let (sender, receiver) = oneshot::channel();
    sampling_relay
        .send(DaSamplingServiceMsg::GetValidatedBlobs {
            reply_channel: sender,
        })
        .await
        .map_err(|(error, _)| Box::new(error) as DynError)?;
    receiver.await.map_err(|error| Box::new(error) as DynError)
}

fn handle_inbound_message(
    msg: LeaderMsg,
    winning_pol_epoch_slots_sender: &watch::Sender<Option<WinningPolInfo>>,
) {
    let LeaderMsg::WinningPolEpochSlotStreamSubscribe { sender } = msg;

    sender
        .send(winning_pol_epoch_slots_sender.subscribe())
        .unwrap_or_else(|_| {
            error!("Could not subscribe to POL epoch winning slots channel.");
        });
}
