pub mod libp2p;
pub mod mock;

use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    pin::Pin,
};

use ::libp2p::PeerId;
use futures::Stream;
use nomos_core::{da::BlobId, header::HeaderId, sdp::ProviderId};
use nomos_da_network_core::{
    addressbook::AddressBookHandler, protocols::sampling::opinions::OpinionEvent,
    swarm::BalancerStats,
};
use overwatch::{overwatch::handle::OverwatchHandle, services::state::ServiceState};
use subnetworks_assignations::MembershipHandler;
use tokio::sync::{broadcast, mpsc::UnboundedSender};

use crate::SessionStatus;

pub enum ConnectionStatus {
    Ready,
    InsufficientSubnetworkConnections,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ProcessingError {
    #[error("Network backend doesn't have enough subnetwork peers connected")]
    InsufficientSubnetworkConnections,
    #[error("Current session doesn't have enough members")]
    InsufficientSessionMembers,
}

#[async_trait::async_trait]
pub trait NetworkBackend<RuntimeServiceId> {
    type Settings: Clone + Debug + Send + Sync + 'static;
    type State: ServiceState<Settings = Self::Settings> + Clone + Send + Sync;
    type Message: Debug + Send + Sync + 'static;
    type EventKind: Debug + Send + Sync + 'static;
    type NetworkEvent: Debug + Send + Sync + 'static;
    type Membership: MembershipHandler + Clone;
    type HistoricMembership: MembershipHandler + Clone;
    type Addressbook: AddressBookHandler + Clone;

    fn new(
        config: Self::Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        membership: Self::Membership,
        addressbook: Self::Addressbook,
        subnet_refresh_sender: &broadcast::Sender<()>,
        blancer_stats_sender: UnboundedSender<BalancerStats>,
        opinion_sender: UnboundedSender<OpinionEvent>,
    ) -> Self;
    fn shutdown(&mut self);
    async fn process(&self, msg: Self::Message);
    fn update_connection_status(&mut self, status: ConnectionStatus);
    fn update_session_status(&mut self, status: SessionStatus);
    async fn subscribe(
        &mut self,
        event: Self::EventKind,
    ) -> Pin<Box<dyn Stream<Item = Self::NetworkEvent> + Send>>;
    async fn start_historic_sampling(
        &self,
        block_id: HeaderId,
        blob_ids: HashMap<Self::HistoricMembership, HashSet<BlobId>>,
    );
    async fn start_historic_commitments(
        &self,
        block_id: HeaderId,
        blob_id: BlobId,
        session: Self::HistoricMembership,
    );

    fn local_peer_id(&self) -> (PeerId, ProviderId);
}
