pub mod api;
mod blend;
mod kms;
mod leadership;
mod mempool;
mod relays;
mod wallet;

use core::fmt::Debug;
use std::{fmt::Display, iter, pin::Pin, time::Duration};

use futures::{StreamExt as _, stream};
use lb_chain_network_service::api::{ChainNetworkServiceApi, ChainNetworkServiceData};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    block::{Block, Error as BlockError, MAX_TRANSACTIONS},
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, SignedMantleTx, Transaction, TxHash, TxSelect,
        gas::MainnetGasConstants, ops::leader_claim::LeaderClaimOp,
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate},
};
use lb_cryptarchia_engine::{Epoch, Slot};
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
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::{oneshot, watch};
use tracing::{Level, debug, error, info, instrument, span};
use tracing_futures::Instrument as _;

pub use crate::wallet::LeaderWalletConfig;
use crate::{
    blend::BlendAdapter,
    kms::PreloadKmsService,
    leadership::{PotentialWinningPoLSlotNotifier, build_proof_for},
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
    relays::CryptarchiaConsensusRelays,
    wallet::{LeaderWalletError, fund_and_sign_leader_claim_tx},
};

pub(crate) type WinningPolInfo = (LeaderPrivate, Epoch);

const SERVICE_ID: &str = "ChainLeader";

pub(crate) const LOG_TARGET: &str = "chain-leader::service";

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
    Wallet(#[from] WalletApiError),
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

#[derive(Debug)]
pub enum LeaderMsg {
    /// Request a new receiver that yields PoL-winning slot information.
    ///
    /// The stream will yield items in one of the following cases:
    /// * a new epoch starts -> immediately the first potential winning slot of
    ///   the new epoch, if any
    /// * this service is started mid-epoch -> immediately the first potential
    ///   winning slot of the ongoing epoch (the slot can also be in the past
    ///   compared to the current slot as returned by the time service), if any
    /// * a new consumer subscribes -> the latest value that was sent to all the
    ///   other consumers, if any
    PotentialWinningPolEpochSlotStreamSubscribe {
        sender: oneshot::Sender<watch::Receiver<Option<WinningPolInfo>>>,
    },
    Claim {
        sender: oneshot::Sender<Result<(), Error>>,
    },
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
    winning_pol_epoch_slots_sender: watch::Sender<Option<WinningPolInfo>>,
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
            Message = lb_blend_service::message::ServiceMessage<BlendService::BroadcastSettings>,
        > + lb_blend_service::ServiceComponents
        + Send
        + Sync
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
    CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item>,
    ChainNetwork: ChainNetworkServiceData<Tx = Mempool::Item>,
    Wallet: lb_wallet_service::api::WalletServiceData,
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

        let mut winning_pol_slot_notifier = PotentialWinningPoLSlotNotifier::new(
            &ledger_config,
            &self.winning_pol_epoch_slots_sender,
        );

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

        // Wait for other service (except ChainLeader) to become ready, with timeout.
        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            BlendService,
            TxMempoolService<_, _, _, _>,
            TimeService<_, _>,
            CryptarchiaService,
            ChainNetwork,
            Wallet,
            PreloadKmsService<_>
        )
        .await?;
        // Wait for ChainLeader service to become ready.
        // No timeout since it becomes ready only after IBD is complete.
        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            None,
            ChainNetwork
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
                        info!("Received SlotTick for slot {}, ep {}", u64::from(slot), u32::from(epoch));
                        let (tip, tip_state) = match Self::get_tip_ledger_state(&cryptarchia_api).await {
                            Ok(output) => output,
                            Err(e) => {
                                error!("Failed to get tip ledger state: {e:?}");
                                continue;
                            }
                        };
                        let parent = tip;

                        let latest_tree = tip_state.latest_utxos();

                        let epoch_state = match cryptarchia_api.get_epoch_state(slot).await {
                            Ok(Ok(state)) => state,
                            Ok(Err(e)) => {
                                error!("trying to propose a block for slot {} but epoch state is not available: {e}", u64::from(slot));
                                continue;
                            }
                            Err(e) => {
                                error!("Failed to get epoch state: {e}");
                                continue;
                            }
                        };

                        let eligible_utxos = match wallet_api.get_leader_aged_notes(Some(parent)).await {
                            Ok(utxos) => utxos,
                            Err(e) => {
                                error!("Failed to fetch leader aged notes from wallet: {:?}", e);
                                continue;
                            }
                        };

                        let eligible: Vec<_> = match &ledger_config.faucet_pk {
                            Some(fpk) => eligible_utxos.response.into_iter()
                                .filter(|u| u.utxo.note.pk != *fpk).collect(),
                            None => eligible_utxos.response,
                        };

                        // If it's a new epoch or the service just started, pre-compute the first winning slot and notify consumers.
                        winning_pol_slot_notifier.process_epoch(&eligible, latest_tree, &epoch_state, &kms_api).await;

                       if let Some((proof, signing_key)) = build_proof_for(&eligible, latest_tree, &epoch_state, slot, &winning_pol_slot_notifier, &wallet_api, &kms_api).await {
                            // TODO: spawn as a separate task?
                            match Self::propose_block(
                                parent,
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
                                    error!(target: LOG_TARGET, "{e}");
                                }
                            }
                        }
                    }

                    Some(msg) = self.service_resources_handle.inbound_relay.next() => {
                        Self::handle_inbound_message(msg, &self.winning_pol_epoch_slots_sender, &cryptarchia_api, &wallet_api, &wallet_config, relays.mempool_adapter()).await;
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
            Message = lb_blend_service::message::ServiceMessage<BlendService::BroadcastSettings>,
        > + lb_blend_service::ServiceComponents
        + Send
        + Sync
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
    CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item>,
    ChainNetwork: ChainNetworkServiceData<Tx = Mempool::Item>,
    Wallet: lb_wallet_service::api::WalletServiceData,
    RuntimeServiceId: Debug + Display + Sync + Send + 'static + AsServiceId<Wallet>,
{
    #[expect(clippy::allow_attributes_without_reason)]
    #[expect(
        clippy::too_many_arguments,
        reason = "All arguments are required for proposing a block"
    )]
    #[instrument(
        level = "debug",
        skip(tx_selector, relays, ledger_state, ledger_config)
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

        let mut tx_stream: Pin<Box<_>> = Box::pin(txs_stream);

        ledger_state = ledger_state
            .clone()
            .try_apply_header::<Groth16LeaderProof, HeaderId>(slot, &proof, ledger_config)?;

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
        debug!(target: LOG_TARGET, "Successfully applied our own proposed block. Publishing it to the blend network: {:?}", block.header().id());

        blend_adapter.publish_proposal(block.to_proposal()).await;
    }

    async fn handle_inbound_message(
        msg: LeaderMsg,
        winning_pol_epoch_slots_sender: &watch::Sender<Option<WinningPolInfo>>,
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        config: &LeaderWalletConfig,
        mempool: &MempoolAdapter<Mempool::Item>,
    ) {
        match msg {
            LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { sender } => {
                sender
                    .send(winning_pol_epoch_slots_sender.subscribe())
                    .unwrap_or_else(|_| {
                        error!("Could not subscribe to POL epoch winning slots channel.");
                    });
            }
            LeaderMsg::Claim { sender } => {
                Self::handle_claim_message(cryptarchia, wallet, config, mempool, sender).await;
            }
        }
    }

    async fn handle_claim_message(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
        wallet: &WalletApi<Wallet, RuntimeServiceId>,
        config: &LeaderWalletConfig,
        mempool: &MempoolAdapter<Mempool::Item>,
        resp_tx: oneshot::Sender<Result<(), Error>>,
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
    ) -> Result<(), Error> {
        let (tip, ledger_state) = Self::get_tip_ledger_state(cryptarchia).await?;

        let voucher_nullifier = wallet
            .get_claimable_voucher(Some(tip))
            .await?
            .response
            .ok_or(Error::NoClaimableVoucher)?
            .nullifier;

        let signed_tx = fund_and_sign_leader_claim_tx(
            LeaderClaimOp {
                rewards_root: ledger_state.mantle_ledger().claimable_vouchers_root(),
                voucher_nullifier,
            },
            tip,
            wallet,
            config,
        )
        .await?;

        mempool.post_tx(signed_tx).await.map_err(Error::Mempool)
    }

    async fn get_tip_ledger_state(
        cryptarchia: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    ) -> Result<(HeaderId, LedgerState), Error> {
        let tip = cryptarchia.info().await?.tip;
        let ledger_state = cryptarchia
            .get_ledger_state(tip)
            .await?
            .ok_or(Error::LedgerStateNotFound(tip))?;
        Ok((tip, ledger_state))
    }
}
