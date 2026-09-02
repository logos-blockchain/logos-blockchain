/// Re-export for `OpenAPI`
#[cfg(feature = "openapi")]
pub mod openapi {
    pub use crate::backend::Status;
}

use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
    pin::Pin,
    time::Duration,
};

use futures::StreamExt as _;
use lb_core::{
    block::MAX_BLOCK_TRANSACTIONS_SIZE,
    mantle::{
        traits::{Hashable, StorageSize},
        transactions::hash::PrefixedKey,
    },
};
use lb_log_targets::mempool;
use lb_network_service::{NetworkService, message::BackendNetworkMsg};
use lb_services_utils::{
    overwatch::{RecoveryOperator, recovery::operators::RecoveryBackend as RecoveryBackendTrait},
    wait_until_services_are_ready,
};
use lb_storage_service::{StorageService, recovery::StorageRecoveryBackend};
use lb_utils::tokio::task::spawn;
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, relay::OutboundRelay},
};
use tokio::sync::{broadcast, oneshot};

use crate::{
    MempoolMetrics, MempoolMsg, TxsWithCommonPrefix,
    backend::{self, MemPool as MemPoolTrait, MempoolError, RecoverableMempool},
    network::NetworkAdapter as NetworkAdapterTrait,
    storage::MempoolStorageAdapter,
    tx::{settings::TxMempoolSettings, state::TxMempoolState},
};

const LOG_TARGET: &str = mempool::SERVICE;

/// How many accepted transactions a subscriber may fall behind by before it
/// starts missing them.
const ACCEPTED_ITEMS_BUFFER: usize = 64;

type MempoolStateUpdater<Pool, NetworkAdapter, RuntimeServiceId> =
    overwatch::services::state::StateUpdater<
        Option<
            TxMempoolState<
                <Pool as RecoverableMempool>::RecoveryState,
                <Pool as MemPoolTrait>::Settings,
                <NetworkAdapter as NetworkAdapterTrait<RuntimeServiceId>>::Settings,
            >,
        >,
    >;

type TxMempoolRecoveryState<Pool, NetworkAdapter, RuntimeServiceId> = TxMempoolState<
    <Pool as RecoverableMempool>::RecoveryState,
    <Pool as MemPoolTrait>::Settings,
    <NetworkAdapter as NetworkAdapterTrait<RuntimeServiceId>>::Settings,
>;

type TxMempoolRecoverySettings<Pool, NetworkAdapter, RuntimeServiceId> = TxMempoolSettings<
    <Pool as MemPoolTrait>::Settings,
    <NetworkAdapter as NetworkAdapterTrait<RuntimeServiceId>>::Settings,
>;

type TxMempoolRecoveryBackend<Pool, NetworkAdapter, StorageAdapter, RuntimeServiceId> =
    StorageRecoveryBackend<
        TxMempoolRecoveryState<Pool, NetworkAdapter, RuntimeServiceId>,
        TxMempoolRecoverySettings<Pool, NetworkAdapter, RuntimeServiceId>,
        <StorageAdapter as MempoolStorageAdapter<RuntimeServiceId>>::Backend,
        RuntimeServiceId,
    >;

/// A tx mempool service that stores recovery state in its storage backend.
pub type TxMempoolService<MempoolNetworkAdapter, Pool, StorageAdapter, RuntimeServiceId> =
    GenericTxMempoolService<
        Pool,
        MempoolNetworkAdapter,
        TxMempoolRecoveryBackend<Pool, MempoolNetworkAdapter, StorageAdapter, RuntimeServiceId>,
        StorageAdapter,
        RuntimeServiceId,
    >;

/// A generic tx mempool service which wraps around a mempool, a network
/// adapter, and a recovery backend.
pub struct GenericTxMempoolService<
    Pool,
    NetworkAdapter,
    RecoveryBackend,
    StorageAdapter,
    RuntimeServiceId,
> where
    Pool: MemPoolTrait<Storage = StorageAdapter> + RecoverableMempool + Send + Sync,
    Pool::Key: PrefixedKey,
    StorageAdapter: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    <Pool as MemPoolTrait>::Settings: Clone,
    NetworkAdapter: NetworkAdapterTrait<RuntimeServiceId> + Send + Sync,
    NetworkAdapter::Settings: Clone,
    RecoveryBackend: RecoveryBackendTrait<RuntimeServiceId> + Send + Sync,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    initial_state: <Self as ServiceData>::State,
    _phantom: PhantomData<(Pool, NetworkAdapter, RecoveryBackend, StorageAdapter)>,
}

impl<Pool, NetworkAdapter, RecoveryBackend, StorageAdapter, RuntimeServiceId>
    GenericTxMempoolService<Pool, NetworkAdapter, RecoveryBackend, StorageAdapter, RuntimeServiceId>
where
    Pool: MemPoolTrait<Storage = StorageAdapter> + RecoverableMempool + Send + Sync,
    Pool::Key: PrefixedKey,
    StorageAdapter: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    <Pool as MemPoolTrait>::Settings: Clone,
    NetworkAdapter: NetworkAdapterTrait<RuntimeServiceId> + Send + Sync,
    NetworkAdapter::Settings: Clone,
    RecoveryBackend: RecoveryBackendTrait<RuntimeServiceId> + Send + Sync,
{
    pub const fn new(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: <Self as ServiceData>::State,
    ) -> Self {
        Self {
            service_resources_handle,
            initial_state,
            _phantom: PhantomData,
        }
    }
}

impl<Pool, NetworkAdapter, RecoveryBackend, StorageAdapter, RuntimeServiceId> ServiceData
    for GenericTxMempoolService<
        Pool,
        NetworkAdapter,
        RecoveryBackend,
        StorageAdapter,
        RuntimeServiceId,
    >
where
    Pool: MemPoolTrait<Storage = StorageAdapter> + RecoverableMempool + Send + Sync,
    Pool::Key: PrefixedKey,
    StorageAdapter: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    <Pool as MemPoolTrait>::Settings: Clone,
    NetworkAdapter: NetworkAdapterTrait<RuntimeServiceId> + Send + Sync,
    NetworkAdapter::Settings: Clone,
    RecoveryBackend: RecoveryBackendTrait<RuntimeServiceId> + Send + Sync,
{
    type Settings = TxMempoolSettings<<Pool as MemPoolTrait>::Settings, NetworkAdapter::Settings>;
    type State = TxMempoolState<
        <Pool as RecoverableMempool>::RecoveryState,
        <Pool as MemPoolTrait>::Settings,
        NetworkAdapter::Settings,
    >;
    type StateOperator = RecoveryOperator<RecoveryBackend>;
    type Message = MempoolMsg<Pool::BlockId, Pool::Item, Pool::Item, Pool::Key>;
}

#[async_trait::async_trait]
impl<Pool, NetworkAdapter, RecoveryBackend, StorageAdapter, RuntimeServiceId>
    ServiceCore<RuntimeServiceId>
    for GenericTxMempoolService<
        Pool,
        NetworkAdapter,
        RecoveryBackend,
        StorageAdapter,
        RuntimeServiceId,
    >
where
    Pool: MemPoolTrait<Storage = StorageAdapter> + RecoverableMempool + Send + Sync,
    StorageAdapter: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    <Pool as RecoverableMempool>::RecoveryState: Debug + Send + Sync,
    Pool::Item: Hashable<Hash = Pool::Key> + StorageSize + Clone + Send + 'static,
    Pool::Key: PrefixedKey<Prefix: Send + Sync>,
    Pool::Settings: Clone + Sync + Send,
    NetworkAdapter:
        NetworkAdapterTrait<RuntimeServiceId, Payload = Pool::Item, Key = Pool::Key> + Send + Sync,
    NetworkAdapter::Settings: Clone + Send + Sync + 'static,
    RecoveryBackend: RecoveryBackendTrait<RuntimeServiceId> + Send + Sync,
    RuntimeServiceId: Display
        + Debug
        + Sync
        + Send
        + 'static
        + AsServiceId<Self>
        + AsServiceId<NetworkService<NetworkAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<
            StorageService<
                <StorageAdapter as MempoolStorageAdapter<RuntimeServiceId>>::Backend,
                RuntimeServiceId,
            >,
        >,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        tracing::trace!(
            target: LOG_TARGET,
            "Initializing TxMempoolService with initial state {:#?}",
            initial_state.pool
        );
        Ok(Self::new(service_resources_handle, initial_state))
    }

    async fn run(mut self) -> Result<(), overwatch::DynError> {
        let settings_handle = &self.service_resources_handle.settings_handle;
        let settings = settings_handle.notifier().get_updated_settings();

        let overwatch_handle = &self.service_resources_handle.overwatch_handle;

        let storage_relay = overwatch_handle
            .relay::<StorageService<
                <StorageAdapter as MempoolStorageAdapter<RuntimeServiceId>>::Backend,
                RuntimeServiceId,
            >>()
            .await
            .expect("Storage service relay should be available");

        let storage_adapter =
            <StorageAdapter as MempoolStorageAdapter<RuntimeServiceId>>::new(storage_relay);

        let pool_state = self.initial_state.pool.take();

        let mut pool = match pool_state {
            None => <Pool as MemPoolTrait>::new(settings.pool.clone(), storage_adapter),
            Some(recovered_pool_state) => <Pool as RecoverableMempool>::recover(
                settings.pool.clone(),
                recovered_pool_state,
                storage_adapter,
            ),
        };

        let network_service_relay = overwatch_handle
            .relay::<NetworkService<_, _>>()
            .await
            .expect("Relay connection with NetworkService should succeed");

        // Queue for network messages
        let mut network_items = NetworkAdapter::new(
            settings_handle
                .notifier()
                .get_updated_settings()
                .network_adapter,
            network_service_relay.clone(),
        )
        .await
        .payload_stream()
        .await;

        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            NetworkService<_, _>
        )
        .await?;

        let (accepted_items_channel_sender, _) = broadcast::channel(ACCEPTED_ITEMS_BUFFER);

        self.run_event_loop(
            &mut pool,
            network_service_relay,
            &mut network_items,
            &accepted_items_channel_sender,
        )
        .await
    }
}

impl<Pool, NetworkAdapter, RecoveryBackend, StorageAdapter, RuntimeServiceId>
    GenericTxMempoolService<Pool, NetworkAdapter, RecoveryBackend, StorageAdapter, RuntimeServiceId>
where
    Pool: MemPoolTrait<Storage = StorageAdapter> + RecoverableMempool + Send + Sync,
    StorageAdapter: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Pool::Item: Hashable<Hash = Pool::Key> + StorageSize + Clone + Send + 'static,
    Pool::Key: PrefixedKey<Prefix: Send + Sync>,
    Pool::Settings: Clone,
    NetworkAdapter: NetworkAdapterTrait<RuntimeServiceId, Payload = Pool::Item> + Send + Sync,
    NetworkAdapter::Settings: Clone + Send + 'static,
    RecoveryBackend: RecoveryBackendTrait<RuntimeServiceId> + Send + Sync,
    RuntimeServiceId: 'static,
{
    async fn run_event_loop(
        &mut self,
        pool: &mut Pool,
        network_service_relay: OutboundRelay<
            BackendNetworkMsg<NetworkAdapter::Backend, RuntimeServiceId>,
        >,
        network_items: &mut Box<dyn futures::Stream<Item = (Pool::Key, Pool::Item)> + Unpin + Send>,
        accepted_items_channel_sender: &broadcast::Sender<Pool::Item>,
    ) -> Result<(), overwatch::DynError>
    where
        Pool::Settings: Send + Sync,
        NetworkAdapter::Settings: Send + Sync,
    {
        loop {
            tokio::select! {
                // Queue for relay messages
                Some(relay_msg) = self.service_resources_handle.inbound_relay.recv() => {
                    let state_updater = self.service_resources_handle.state_updater.clone();
                    let settings = self
                        .service_resources_handle
                        .settings_handle
                        .notifier()
                        .get_updated_settings()
                        .network_adapter;

                    Self::handle_mempool_message(pool, relay_msg, network_service_relay.clone(), state_updater, settings, accepted_items_channel_sender).await;
                }
                Some((key, item)) = network_items.next() => {
                    Self::handle_network_item(pool, key, item, &self.service_resources_handle.state_updater, accepted_items_channel_sender).await;
                }
            }
        }
    }

    async fn handle_mempool_message(
        pool: &mut Pool,
        message: MempoolMsg<Pool::BlockId, Pool::Item, Pool::Item, Pool::Key>,
        network_relay: OutboundRelay<BackendNetworkMsg<NetworkAdapter::Backend, RuntimeServiceId>>,
        state_updater: MempoolStateUpdater<Pool, NetworkAdapter, RuntimeServiceId>,
        settings: NetworkAdapter::Settings,
        accepted_items_channel_sender: &broadcast::Sender<Pool::Item>,
    ) where
        Pool::Settings: Send + Sync,
        NetworkAdapter::Settings: Send + Sync,
    {
        match message {
            MempoolMsg::Add {
                payload,
                key,
                reply_channel,
            } => {
                Self::handle_add_message(
                    pool,
                    key,
                    payload,
                    reply_channel,
                    network_relay,
                    state_updater,
                    settings,
                    accepted_items_channel_sender,
                )
                .await;
            }
            MempoolMsg::View {
                ancestor_hint,
                reply_channel,
            } => {
                Self::handle_view_message(pool, ancestor_hint, reply_channel).await;
            }
            MempoolMsg::GetTransactionsByPrefix {
                prefix,
                reply_channel,
            } => {
                let result = Self::get_transactions_by_prefix(pool, &prefix).await;

                if let Err(_e) = reply_channel.send(result) {
                    tracing::debug!(target: LOG_TARGET, "Failed to send prefix lookup reply");
                }
            }
            MempoolMsg::Remove { ids } => {
                pool.remove(&ids).await;
            }
            MempoolMsg::Metrics { reply_channel } => {
                Self::handle_metrics_message(pool, reply_channel);
            }
            MempoolMsg::Status {
                items,
                reply_channel,
            } => {
                Self::handle_status_message(pool, &items, reply_channel);
            }
            MempoolMsg::SubscribeToAccepted { reply_channel } => {
                if reply_channel
                    .send(accepted_items_channel_sender.subscribe())
                    .is_err()
                {
                    tracing::warn!(target: LOG_TARGET, "Subscriber hung up before it could be given the accepted-item stream.");
                }
            }
        }
    }

    #[expect(clippy::too_many_arguments, reason = "categorize args")]
    async fn handle_add_message(
        pool: &mut Pool,
        key: Pool::Key,
        item: Pool::Item,
        reply_channel: oneshot::Sender<Result<(), MempoolError>>,
        network_relay: OutboundRelay<BackendNetworkMsg<NetworkAdapter::Backend, RuntimeServiceId>>,
        state_updater: MempoolStateUpdater<Pool, NetworkAdapter, RuntimeServiceId>,
        settings: NetworkAdapter::Settings,
        accepted_items_channel_sender: &broadcast::Sender<Pool::Item>,
    ) where
        Pool::Settings: Send + Sync,
        NetworkAdapter::Settings: Send + Sync,
    {
        if let Err(error) = Self::validate_item_for_mempool(&item) {
            Self::handle_add_error(error, reply_channel);
            return;
        }

        match pool.add_item(key, item.clone()).await {
            Ok(_id) => {
                Self::notify_about_accepted_item(accepted_items_channel_sender, item.clone());
                Self::handle_add_success(
                    pool,
                    &state_updater,
                    settings,
                    network_relay,
                    item,
                    reply_channel,
                );
            }
            Err(MempoolError::ExistingItem) => {
                // Tx already in pool, but since this came from a local submission
                // (not gossip), re-gossip it so leader nodes can pick it up.
                Self::notify_about_accepted_item(accepted_items_channel_sender, item.clone());
                spawn("logos/mempool/transaction-regossip", async move {
                    let adapter = NetworkAdapter::new(settings, network_relay).await;
                    adapter.send(item).await;
                });
                if let Err(e) = reply_channel.send(Ok(())) {
                    tracing::debug!(target: LOG_TARGET, "Failed to send add reply: {:?}", e);
                }
            }
            Err(e) => Self::handle_add_error(e, reply_channel),
        }
    }

    async fn handle_view_message(
        pool: &Pool,
        ancestor_hint: Pool::BlockId,
        reply_channel: oneshot::Sender<Pin<Box<dyn futures::Stream<Item = Pool::Item> + Send>>>,
    ) {
        let pending_items = pool.pending_item_count();
        tracing::trace!(target: LOG_TARGET, pending_items, "Handling mempool View message");

        let items = pool
            .view(ancestor_hint)
            .await
            .unwrap_or_else(|_| Box::pin(futures::stream::iter(Vec::new())));

        if let Err(_e) = reply_channel.send(Box::pin(items)) {
            tracing::debug!(target: LOG_TARGET, "Failed to send view reply");
        }
    }

    fn handle_metrics_message(pool: &Pool, reply_channel: oneshot::Sender<MempoolMetrics>) {
        let info = MempoolMetrics {
            pending_items: pool.pending_item_count(),
            last_item_timestamp: pool.last_item_timestamp(),
        };

        if let Err(_e) = reply_channel.send(info) {
            tracing::debug!(target: LOG_TARGET, "Failed to send metrics reply");
        }
    }

    fn handle_status_message(
        pool: &Pool,
        items: &[Pool::Key],
        reply_channel: oneshot::Sender<Vec<backend::Status>>,
    ) {
        let statuses = pool.status(items);

        if let Err(_e) = reply_channel.send(statuses) {
            tracing::debug!(target: LOG_TARGET, "Failed to send status reply");
        }
    }

    /// Every transaction whose hash starts with `prefix`.
    ///
    /// The prefix index is consulted first, so the storage round-trip only
    /// covers keys that actually match. No policy is applied here: what a
    /// non-unique match means is a consensus question, so it belongs to the
    /// caller.
    async fn get_transactions_by_prefix(
        pool: &Pool,
        prefix: &<Pool::Key as PrefixedKey>::Prefix,
    ) -> Result<TxsWithCommonPrefix<Pool::Item>, MempoolError> {
        let keys: Vec<Pool::Key> = pool.keys_by_prefix(prefix).cloned().collect();

        pool.get_items_by_keys(keys)
            .await
            .map_err(|e| MempoolError::StorageError(format!("Failed to get items by keys: {e:?}")))
    }

    fn handle_add_success(
        pool: &Pool,
        state_updater: &MempoolStateUpdater<Pool, NetworkAdapter, RuntimeServiceId>,
        settings: NetworkAdapter::Settings,
        network_relay: OutboundRelay<BackendNetworkMsg<NetworkAdapter::Backend, RuntimeServiceId>>,
        item_for_broadcast: Pool::Item,
        reply_channel: oneshot::Sender<Result<(), MempoolError>>,
    ) {
        state_updater.update(Some(<Pool as RecoverableMempool>::save(pool).into()));

        spawn("logos/mempool/transaction-broadcast", async move {
            let adapter = NetworkAdapter::new(settings, network_relay).await;
            adapter.send(item_for_broadcast).await;
        });

        if let Err(e) = reply_channel.send(Ok(())) {
            tracing::debug!(target: LOG_TARGET, "Failed to send add reply: {:?}", e);
        }
    }

    fn handle_add_error(
        error: MempoolError,
        reply_channel: oneshot::Sender<Result<(), MempoolError>>,
    ) {
        tracing::debug!(target: LOG_TARGET, "Could not add item to the pool: {}", error);
        if let Err(e) = reply_channel.send(Err(error)) {
            tracing::debug!(target: LOG_TARGET, "Failed to send error reply: {:?}", e);
        }
    }

    fn notify_about_accepted_item(
        accepted_items_channel_sender: &broadcast::Sender<Pool::Item>,
        item: Pool::Item,
    ) {
        if accepted_items_channel_sender.receiver_count() > 0 {
            drop(accepted_items_channel_sender.send(item));
        }
    }

    fn validate_item_for_mempool(item: &Pool::Item) -> Result<(), MempoolError> {
        let size = item.storage_size();
        if size > MAX_BLOCK_TRANSACTIONS_SIZE {
            return Err(MempoolError::ItemTooLarge {
                size,
                max: MAX_BLOCK_TRANSACTIONS_SIZE,
            });
        }

        Ok(())
    }

    async fn handle_network_item(
        pool: &mut Pool,
        key: Pool::Key,
        item: Pool::Item,
        state_updater: &MempoolStateUpdater<Pool, NetworkAdapter, RuntimeServiceId>,
        accepted_items_channel_sender: &broadcast::Sender<Pool::Item>,
    ) where
        Pool::Settings: Send + Sync,
        NetworkAdapter::Settings: Send + Sync,
    {
        if let Err(err) = Self::validate_item_for_mempool(&item) {
            tracing::debug!(
                target: LOG_TARGET,
                "could not add network item to the pool due to: {err}"
            );
            return;
        }

        let accepted_item = item.clone();
        if let Err(e) = pool.add_item(key, item).await {
            Self::handle_network_add_error(e);
            return;
        }
        Self::notify_about_accepted_item(accepted_items_channel_sender, accepted_item);

        tracing::trace!(
            target: LOG_TARGET,
            {
                counter.tx_mempool_pending_items = pool.pending_item_count(),
            },
            "mempool pending items updated"
        );

        state_updater.update(Some(<Pool as RecoverableMempool>::save(pool).into()));
    }

    fn handle_network_add_error(error: MempoolError) {
        match error {
            MempoolError::ExistingItem => {
                tracing::trace!(
                    target: LOG_TARGET,
                    "network item already exists in the mempool"
                );
            }
            err => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "could not add item to the pool due to: {err}"
                );
            }
        }
    }
}
