pub mod backend;
pub mod mempool;
pub mod network;
pub mod storage;

use std::{
    error::Error,
    fmt::{Debug, Display, Formatter},
    hash::Hash,
    sync::Arc,
    time::{Duration, Instant},
};

use backend::{
    trigger::{MempoolPublishTrigger, MempoolPublishTriggerConfig, ShareEvent},
    tx::mock::MockTxVerifier,
};
use lb_core::da::blob::Share;
use lb_da_network_core::swarm::DispersalValidationError;
use lb_da_network_service::{
    NetworkService, membership::MembershipAdapter, storage::MembershipStorageAdapter,
};
use lb_services_utils::wait_until_services_are_ready;
use lb_storage_service::StorageService;
use lb_subnetworks_assignations::MembershipHandler;
use lb_tracing::info_with_id;
use lb_tx_service::backend::MempoolError;
use mempool::{DaMempoolAdapter, MempoolAdapterError};
use network::NetworkAdapter;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize};
use storage::DaStorageAdapter;
use tokio::sync::oneshot::Sender;
use tokio_stream::StreamExt as _;
use tracing::{error, instrument};

use crate::{
    backend::{TxVerifierBackend, VerifierBackend},
    network::ValidationRequest,
};

pub type DaVerifierService<ShareVerifier, Network, Storage, MempoolAdapter, RuntimeServiceId> =
    GenericDaVerifierService<
        ShareVerifier,
        MockTxVerifier,
        Network,
        Storage,
        MempoolAdapter,
        RuntimeServiceId,
    >;

pub enum DaVerifierMsg<Commitments, LightShare, Share, Answer> {
    AddShare {
        share: Share,
        reply_channel: Sender<Option<Answer>>,
    },
    VerifyShare {
        commitments: Arc<Commitments>,
        light_share: Box<LightShare>,
        reply_channel: Sender<Result<(), DynError>>,
    },
}

impl<C: 'static, L: 'static, B: 'static, A: 'static> Debug for DaVerifierMsg<C, L, B, A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddShare { .. } => {
                write!(f, "DaVerifierMsg::AddShare")
            }
            Self::VerifyShare { .. } => {
                write!(f, "DaVerifierMsg::VerifyShare")
            }
        }
    }
}

pub struct GenericDaVerifierService<
    ShareVerifier,
    TxVerifier,
    Network,
    Storage,
    MempoolAdapter,
    RuntimeServiceId,
> where
    ShareVerifier: VerifierBackend,
    ShareVerifier::Settings: Clone,
    ShareVerifier::DaShare: 'static,
    TxVerifier: TxVerifierBackend,
    TxVerifier::Settings: Clone,
    Network: NetworkAdapter<RuntimeServiceId>,
    Network::Settings: Clone,
    MempoolAdapter: DaMempoolAdapter,
    Storage: DaStorageAdapter<RuntimeServiceId>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    share_verifier: ShareVerifier,
    tx_verifier: TxVerifier,
}

impl<ShareVerifier, TxVerifier, Network, Storage, MempoolAdapter, RuntimeServiceId>
    GenericDaVerifierService<
        ShareVerifier,
        TxVerifier,
        Network,
        Storage,
        MempoolAdapter,
        RuntimeServiceId,
    >
where
    ShareVerifier: VerifierBackend + Send + Sync + 'static,
    ShareVerifier::DaShare: Debug + Send,
    ShareVerifier::Error: Error + Send + Sync,
    ShareVerifier::Settings: Clone,
    <ShareVerifier::DaShare as Share>::BlobId:
        Clone + Debug + AsRef<[u8]> + Hash + Eq + Send + Sync,
    <ShareVerifier::DaShare as Share>::LightShare: Send,
    <ShareVerifier::DaShare as Share>::SharesCommitments: Send,
    TxVerifier: TxVerifierBackend<BlobId = <ShareVerifier::DaShare as Share>::BlobId> + Send + Sync,
    TxVerifier::Settings: Clone,
    TxVerifier::Tx: Send,
    TxVerifier::Error: Error + Send + Sync + 'static,
    Network: NetworkAdapter<RuntimeServiceId, Share = ShareVerifier::DaShare, Tx = TxVerifier::Tx>
        + Send
        + 'static,
    Network::Settings: Clone,
    MempoolAdapter:
        DaMempoolAdapter<Tx = <TxVerifier as TxVerifierBackend>::Tx> + Send + Sync + 'static,
    Storage: DaStorageAdapter<RuntimeServiceId, Share = ShareVerifier::DaShare, Tx = TxVerifier::Tx>
        + Send
        + Sync
        + 'static,
{
    #[instrument(skip_all)]
    async fn handle_new_share(
        verifier: &ShareVerifier,
        storage_adapter: &Storage,
        mempool_trigger: &mut MempoolPublishTrigger<<ShareVerifier::DaShare as Share>::BlobId>,
        mempool_adapter: &MempoolAdapter,
        share: ShareVerifier::DaShare,
    ) -> Result<(), DynError> {
        if storage_adapter
            .get_share(share.blob_id(), share.share_idx())
            .await?
            .is_some()
        {
            info_with_id!(share.blob_id().as_ref(), "VerifierShareExists");
        } else {
            info_with_id!(share.blob_id().as_ref(), "VerifierAddShare");
            let (blob_id, share_idx) = (share.blob_id(), share.share_idx());
            let (light_share, commitments) = share.into_share_and_commitments();
            // TODO: remove TX if verification fails.
            verifier.verify(&commitments, &light_share)?;
            storage_adapter
                .add_share(blob_id.clone(), share_idx, commitments, light_share)
                .await?;
            let share_status = mempool_trigger.record(blob_id.clone(), ShareEvent::Share);
            if let Some((_, tx)) = storage_adapter.get_tx(blob_id).await?
                && matches!(share_status, backend::trigger::ShareState::Complete)
            {
                mempool_adapter.post_tx(tx).await?;
            }
        }
        Ok(())
    }

    async fn handle_new_tx(
        verifier: &TxVerifier,
        storage_adapter: &Storage,
        mempool_trigger: &mut MempoolPublishTrigger<<ShareVerifier::DaShare as Share>::BlobId>,
        assignations: u16,
        tx: TxVerifier::Tx,
    ) -> Result<(), DynError> {
        let blob_id = verifier.blob_id(&tx)?;
        if storage_adapter.get_tx(blob_id.clone()).await?.is_some() {
            info_with_id!(blob_id.as_ref(), "VerifierTxExists");
        } else {
            info_with_id!(blob_id.as_ref(), "VerifierAddTx");
            verifier.verify(&tx)?;
            storage_adapter
                .add_tx(blob_id.clone(), assignations, tx)
                .await?;
            mempool_trigger.record(blob_id, ShareEvent::Transaction { assignations });
        }
        Ok(())
    }

    async fn prune_pending_txs(
        storage_adapter: &Storage,
        mempool_trigger: &mut MempoolPublishTrigger<<ShareVerifier::DaShare as Share>::BlobId>,
        mempool_adapter: &MempoolAdapter,
    ) -> Result<(), DynError> {
        let now = Instant::now();
        let blob_ids = mempool_trigger.prune(now);
        for blob_id in blob_ids {
            if let Some((_, tx)) = storage_adapter.get_tx(blob_id.clone()).await? {
                match mempool_adapter.post_tx(tx).await {
                    Ok(()) | Err(MempoolAdapterError::Mempool(MempoolError::ExistingItem)) => {}
                    Err(err) => return Err(Box::new(err)),
                }
            }
        }
        Ok(())
    }
}

impl<ShareVerifier, TxVerifier, Network, DaStorage, MempoolAdapter, RuntimeServiceId> ServiceData
    for GenericDaVerifierService<
        ShareVerifier,
        TxVerifier,
        Network,
        DaStorage,
        MempoolAdapter,
        RuntimeServiceId,
    >
where
    ShareVerifier: VerifierBackend,
    ShareVerifier::Settings: Clone,
    TxVerifier: TxVerifierBackend,
    TxVerifier::Settings: Clone,
    Network: NetworkAdapter<RuntimeServiceId>,
    Network::Settings: Clone,
    DaStorage: DaStorageAdapter<RuntimeServiceId>,
    DaStorage::Settings: Clone,
    MempoolAdapter: DaMempoolAdapter,
{
    type Settings = DaVerifierServiceSettings<
        ShareVerifier::Settings,
        TxVerifier::Settings,
        Network::Settings,
        DaStorage::Settings,
    >;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = DaVerifierMsg<
        <ShareVerifier::DaShare as Share>::SharesCommitments,
        <ShareVerifier::DaShare as Share>::LightShare,
        ShareVerifier::DaShare,
        (),
    >;
}

#[async_trait::async_trait]
impl<ShareVerifier, TxVerifier, Network, DaStorage, MempoolAdapter, RuntimeServiceId>
    ServiceCore<RuntimeServiceId>
    for GenericDaVerifierService<
        ShareVerifier,
        TxVerifier,
        Network,
        DaStorage,
        MempoolAdapter,
        RuntimeServiceId,
    >
where
    ShareVerifier: VerifierBackend + Send + Sync + 'static,
    ShareVerifier::Settings: Clone + Send + Sync + 'static,
    ShareVerifier::DaShare: Debug + Send + Sync + 'static,
    ShareVerifier::Error: Error + Send + Sync + 'static,
    <ShareVerifier::DaShare as Share>::BlobId:
        Clone + Debug + AsRef<[u8]> + Hash + Eq + Send + Sync + 'static,
    <ShareVerifier::DaShare as Share>::LightShare: Debug + Send + Sync + 'static,
    <ShareVerifier::DaShare as Share>::SharesCommitments: Debug + Send + Sync + 'static,
    TxVerifier: TxVerifierBackend<BlobId = <ShareVerifier::DaShare as Share>::BlobId> + Send + Sync,
    TxVerifier::Tx: Send,
    TxVerifier::Settings: Clone + Send + Sync + 'static,
    TxVerifier::Error: Error + Send + Sync + 'static,
    Network: NetworkAdapter<RuntimeServiceId, Share = ShareVerifier::DaShare, Tx = TxVerifier::Tx>
        + Send
        + Sync
        + 'static,
    Network::Membership: MembershipHandler + Clone,
    Network::Settings: Clone + Send + Sync + 'static,
    Network::Storage: MembershipStorageAdapter<
            <Network::Membership as MembershipHandler>::Id,
            <Network::Membership as MembershipHandler>::NetworkId,
        > + Send
        + Sync
        + 'static,
    Network::MembershipAdapter: MembershipAdapter,
    DaStorage: DaStorageAdapter<RuntimeServiceId, Share = ShareVerifier::DaShare, Tx = TxVerifier::Tx>
        + Send
        + Sync
        + 'static,
    DaStorage::Settings: Clone + Send + Sync + 'static,
    MempoolAdapter:
        DaMempoolAdapter<Tx = <TxVerifier as TxVerifierBackend>::Tx> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Display
        + Sync
        + Send
        + 'static
        + AsServiceId<Self>
        + AsServiceId<
            NetworkService<
                Network::Backend,
                Network::Membership,
                Network::MembershipAdapter,
                Network::Storage,
                Network::ApiAdapter,
                Network::SdpAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<MempoolAdapter::MempoolService>
        + AsServiceId<StorageService<DaStorage::Backend, RuntimeServiceId>>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let DaVerifierServiceSettings {
            share_verifier_settings,
            tx_verifier_settings,
            ..
        } = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();
        Ok(Self {
            service_resources_handle,
            share_verifier: ShareVerifier::new(share_verifier_settings),
            tx_verifier: TxVerifier::new(tx_verifier_settings),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Run loop contains all handling for readablity"
    )]
    async fn run(self) -> Result<(), DynError> {
        // This service will likely have to be modified later on.
        // Most probably the verifier itself need to be constructed/update for every
        // message with an updated list of the available nodes list, as it needs
        // his own index coming from the position of his bls public key landing
        // in the above-mentioned list.
        let Self {
            mut service_resources_handle,
            share_verifier,
            tx_verifier,
        } = self;

        let DaVerifierServiceSettings {
            network_adapter_settings,
            mempool_trigger_settings,
            ..
        } = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let network_relay = service_resources_handle
            .overwatch_handle
            .relay::<NetworkService<_, _, _, _, _, _, _>>()
            .await?;
        let network_adapter = Network::new(network_adapter_settings, network_relay).await;
        let mut share_stream = network_adapter.share_stream().await;
        let mut tx_stream = network_adapter.tx_stream().await;

        let mempool_relay = service_resources_handle
            .overwatch_handle
            .relay::<MempoolAdapter::MempoolService>()
            .await?;
        let mempool_adapter = MempoolAdapter::new(mempool_relay);

        let storage_relay = service_resources_handle
            .overwatch_handle
            .relay::<StorageService<_, _>>()
            .await?;
        let storage_adapter = DaStorage::new(storage_relay).await;

        service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        let mut prune_interval = tokio::time::interval(mempool_trigger_settings.prune_interval);
        let mut mempool_trigger = MempoolPublishTrigger::new(mempool_trigger_settings);

        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            NetworkService<_, _, _, _, _, _, _>,
            StorageService<_, _>
        )
        .await?;

        loop {
            tokio::select! {
                Some(ValidationRequest{item: share, sender}) = share_stream.next() => {
                    let blob_id = share.blob_id();
                    if let Err(err) =  Self::handle_new_share(
                        &share_verifier,
                        &storage_adapter,
                        &mut mempool_trigger,
                        &mempool_adapter,
                        share
                    ).await {
                        if let Some(sender) = sender
                            && let Err(e) = sender.send(Err(DispersalValidationError)).await {
                                tracing::debug!("Verifier couldn't respond share validation request failure: {e}");
                            }
                        error!("Error handling blob {blob_id:?} due to {err:?}");
                        continue;
                    }
                    if let Some(sender) = sender && let Err(e) = sender.send(Ok(())).await {
                        tracing::debug!("Verifier couldn't respond share validation request success: {e}");
                    }
                }
                Some(ValidationRequest{ item: (assignations, tx), sender }) = tx_stream.next() => {
                    if let Err(err) =  Self::handle_new_tx(
                        &tx_verifier,
                        &storage_adapter,
                        &mut mempool_trigger,
                        assignations,
                        tx,
                    ).await {
                        if let Some(sender) = sender && let Err(e) = sender.send(Err(DispersalValidationError)).await{
                            tracing::debug!("Verifier couldn't respond tx validation request failure: {e}");
                        }
                        error!("Error handling tx due to {err:?}");
                        continue;
                    }
                    if let Some(sender) = sender && let Err(e) = sender.send(Ok(())).await{
                        tracing::debug!("Verifier couldn't respond tx validation request success: {e}");
                    }
                }
                Some(msg) = service_resources_handle.inbound_relay.recv() => {
                    match msg {
                        DaVerifierMsg::AddShare { share, reply_channel } => {
                            let blob_id = share.blob_id();
                            match Self::handle_new_share(
                                &share_verifier,
                                &storage_adapter,
                                &mut mempool_trigger,
                                &mempool_adapter,
                                share
                            ).await {
                                Ok(attestation) => {
                                    if let Err(err) = reply_channel.send(Some(attestation)) {
                                        error!("Error replying attestation {err:?}");
                                    }
                                },
                                Err(err) => {
                                    error!("Error handling blob {blob_id:?} due to {err:?}");
                                    if let Err(err) = reply_channel.send(None) {
                                        error!("Error replying attestation {err:?}");
                                    }
                                },
                            }
                        },
                        DaVerifierMsg::VerifyShare {commitments,  light_share, reply_channel } => {
                            match share_verifier.verify(&commitments, &light_share) {
                                Ok(()) => {
                                    if let Err(err) = reply_channel.send(Ok(())) {
                                        error!("Error replying verification {err:?}");
                                    }
                                },
                                Err(err) => {
                                    error!("Error verifying blob due to {err:?}");
                                    if let Err(err) = reply_channel.send(Err(err.into())) {
                                        error!("Error replying verification {err:?}");
                                    }
                                },
                            }
                        },
                    }
                }
                _ = prune_interval.tick() => {
                    if let Err(err) = Self::prune_pending_txs(
                        &storage_adapter,
                        &mut mempool_trigger,
                        &mempool_adapter
                    ).await {
                        error!("Error pruning txs due to {err:?}");
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaVerifierServiceSettings<
    ShareVerifierSettings,
    TxVerifierSettings,
    NetworkSettings,
    StorageSettings,
> {
    pub share_verifier_settings: ShareVerifierSettings,
    pub tx_verifier_settings: TxVerifierSettings,
    pub network_adapter_settings: NetworkSettings,
    pub storage_adapter_settings: StorageSettings,
    pub mempool_trigger_settings: MempoolPublishTriggerConfig,
}
