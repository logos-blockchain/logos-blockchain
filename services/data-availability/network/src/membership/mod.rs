pub mod adapters;
pub mod handler;

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
};

use futures::Stream;
use lb_core::sdp::{ProviderId, SessionNumber};
use libp2p::Multiaddr;
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use thiserror::Error;

pub type Assignations<Id, NetworkId> = HashMap<NetworkId, HashSet<Id>>;

#[derive(Debug)]
pub struct SubnetworkPeers<Id> {
    pub session_id: SessionNumber,
    pub peers: HashMap<Id, Multiaddr>,
    pub provider_mappings: HashMap<Id, ProviderId>,
}

pub type PeerMultiaddrStream<Id> =
    Pin<Box<dyn Stream<Item = SubnetworkPeers<Id>> + Send + Sync + 'static>>;

#[derive(Error, Debug)]
pub enum MembershipAdapterError {
    #[error("Other error: {0}")]
    Other(#[from] DynError),
}

#[async_trait::async_trait]
pub trait MembershipAdapter {
    type MembershipService: ServiceData;
    type Id;

    fn new(relay: OutboundRelay<<Self::MembershipService as ServiceData>::Message>) -> Self;

    async fn subscribe(&self) -> Result<PeerMultiaddrStream<Self::Id>, MembershipAdapterError>;
}
