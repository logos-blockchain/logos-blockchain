use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
    time::Duration,
};

use adapters::{
    session::service::BroadcastSessionAdapter,
    wallet::{DaWalletAdapter, mock::MockWalletAdapter},
};
use backend::DispersalTask;
use futures::{StreamExt as _, stream::FuturesUnordered};
use lb_core::{
    mantle::{
        Transaction as _,
        ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
        tx_builder::MantleTxBuilder,
    },
    sdp::SessionNumber,
};
use lb_da_network_core::{PeerId, SubnetworkId};
use lb_services_utils::wait_until_services_are_ready;
use lb_subnetworks_assignations::MembershipHandler;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::error;

use crate::{
    adapters::{network::DispersalNetworkAdapter, session::SessionAdapter},
    backend::DispersalBackend,
};

pub mod adapters;
pub mod backend;

#[derive(Error, Debug)]
pub enum DispersalServiceError {
    #[error("Current session info is not available")]
    SessionUnavailable,
}

#[derive(Debug)]
pub enum DaDispersalMsg<B: DispersalBackend> {
    Disperse {
        tx_builder: MantleTxBuilder,
        channel_id: ChannelId,
        parent_msg_id: MsgId,
        signer: Ed25519PublicKey,
        data: Vec<u8>,
        reply_channel: oneshot::Sender<Result<B::BlobId, DynError>>,
    },
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispersalServiceSettings<BackendSettings> {
    pub backend: BackendSettings,
}

pub type DispersalService<Backend, NetworkAdapter, Membership, RuntimeServiceId> =
    GenericDispersalService<
        Backend,
        NetworkAdapter,
        MockWalletAdapter,
        Membership,
        BroadcastSessionAdapter<RuntimeServiceId>,
        RuntimeServiceId,
    >;

pub struct GenericDispersalService<
    Backend,
    NetworkAdapter,
    WalletAdapter,
    Membership,
    Session,
    RuntimeServiceId,
> where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    Backend: DispersalBackend<NetworkAdapter = NetworkAdapter>,
    Backend::BlobId: Serialize,
    Backend::Settings: Clone,
    NetworkAdapter: DispersalNetworkAdapter,
    Session: SessionAdapter,
    WalletAdapter: DaWalletAdapter,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _backend: PhantomData<Backend>,
}

impl<Backend, NetworkAdapter, WalletAdapter, Membership, Session, RuntimeServiceId> ServiceData
    for GenericDispersalService<
        Backend,
        NetworkAdapter,
        WalletAdapter,
        Membership,
        Session,
        RuntimeServiceId,
    >
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    Backend: DispersalBackend<NetworkAdapter = NetworkAdapter>,
    Backend::BlobId: Serialize,
    Backend::Settings: Clone,
    Session: SessionAdapter,
    NetworkAdapter: DispersalNetworkAdapter,
    WalletAdapter: DaWalletAdapter,
{
    type Settings = DispersalServiceSettings<Backend::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = DaDispersalMsg<Backend>;
}

#[async_trait::async_trait]
impl<Backend, NetworkAdapter, WalletAdapter, Membership, Session, RuntimeServiceId>
    ServiceCore<RuntimeServiceId>
    for GenericDispersalService<
        Backend,
        NetworkAdapter,
        WalletAdapter,
        Membership,
        Session,
        RuntimeServiceId,
    >
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    Backend: DispersalBackend<NetworkAdapter = NetworkAdapter, WalletAdapter = WalletAdapter>
        + Send
        + Sync,
    Backend::Settings: Clone + Send + Sync,
    Backend::BlobId: Debug + Serialize,
    NetworkAdapter: DispersalNetworkAdapter<SubnetworkId = Membership::NetworkId> + Send,
    <NetworkAdapter::NetworkService as ServiceData>::Message: 'static,
    Session: SessionAdapter + Send,
    <Session::Service as ServiceData>::Message: 'static,
    WalletAdapter: DaWalletAdapter + Send,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + Send
        + AsServiceId<Self>
        + AsServiceId<NetworkAdapter::NetworkService>
        + AsServiceId<Session::Service>
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
            _backend: PhantomData,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        let Self {
            service_resources_handle,
            ..
        } = self;

        let DispersalServiceSettings {
            backend: backend_settings,
        } = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();
        let network_relay = service_resources_handle
            .overwatch_handle
            .relay::<NetworkAdapter::NetworkService>()
            .await?;
        let network_adapter = NetworkAdapter::new(network_relay);

        let wallet_adapter = WalletAdapter::new();
        let backend = Backend::init(backend_settings, network_adapter, wallet_adapter);

        let session_relay = service_resources_handle
            .overwatch_handle
            .relay::<Session::Service>()
            .await?;
        let session_adapter = Session::new(session_relay);

        let mut inbound_relay = service_resources_handle.inbound_relay;
        let mut disperse_tasks: FuturesUnordered<DispersalTask> = FuturesUnordered::new();

        service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            NetworkAdapter::NetworkService,
            Session::Service
        )
        .await?;

        let mut sessions_stream = session_adapter.subscribe().await?;
        let mut current_session: Option<SessionNumber> = None;

        loop {
            tokio::select! {
                Some(dispersal_msg) = inbound_relay.recv() => {
                    let DaDispersalMsg::Disperse {
                        tx_builder,
                        channel_id,
                        parent_msg_id,
                        signer,
                        data,
                        reply_channel,
                    } = dispersal_msg;
                    let Some(session) = current_session else {
                        if let Err(e) = reply_channel.send(Err(DispersalServiceError::SessionUnavailable.into())) {
                        tracing::error!("Failed to send dispersal error: {e:?}");
                        }
                        continue
                    };
                    match backend.process_dispersal(
                        tx_builder,
                        backend::InitialBlobOpArgs {
                            channel_id,
                            session,
                            parent_msg_id,
                            signer,
                        },
                        data,
                        reply_channel,
                    )
                    .await {
                        Ok(task) => disperse_tasks.push(task),
                        Err(e) => error!("Error while processing dispersal: {e}"),
                    }
                }
                Some(dispersal_result) = disperse_tasks.next() => {
                    if let (channel_id, Some(tx)) = dispersal_result {
                        tracing::info!("Dispersal retry successful for channel: {channel_id:?}, tx: {:?}", tx.hash());
                    } else {
                        tracing::error!("Dispersal failed after all retry attempts");
                    }
                }
                Some(new_session) = sessions_stream.next() => {
                    current_session = Some(new_session);
                }
            }
        }
    }
}
