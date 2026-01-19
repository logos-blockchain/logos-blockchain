use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use lb_subnetworks_assignations::MembershipHandler;
use libp2p::{
    PeerId, Transport as _,
    core::{transport::MemoryTransport, upgrade::Version},
    identity::Keypair,
    swarm::NetworkBehaviour,
};

use crate::{SubnetworkId, addressbook::AddressBookHandler};

#[derive(Clone)]
pub struct AllNeighbours {
    neighbours: Arc<Mutex<HashSet<PeerId>>>,
    addresses: Arc<Mutex<HashMap<PeerId, libp2p::Multiaddr>>>,
}

impl Default for AllNeighbours {
    fn default() -> Self {
        Self::new()
    }
}

impl AllNeighbours {
    #[must_use]
    pub fn new() -> Self {
        Self {
            neighbours: Arc::new(Mutex::new(HashSet::new())),
            addresses: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn add_neighbour(&self, id: PeerId) {
        self.neighbours.lock().unwrap().insert(id);
    }

    pub fn update_addresses(&self, addressbook: Vec<(PeerId, libp2p::Multiaddr)>) {
        self.addresses.lock().unwrap().extend(addressbook);
    }
}

impl MembershipHandler for AllNeighbours {
    type NetworkId = SubnetworkId;
    type Id = PeerId;

    fn membership(&self, _self_id: &Self::Id) -> HashSet<Self::NetworkId> {
        std::iter::once(0).collect()
    }

    fn is_allowed(&self, _id: &Self::Id) -> bool {
        true
    }

    fn members_of(&self, _network_id: &Self::NetworkId) -> HashSet<Self::Id> {
        self.neighbours.lock().unwrap().clone()
    }

    fn members(&self) -> HashSet<Self::Id> {
        self.neighbours.lock().unwrap().clone()
    }

    fn last_subnetwork_id(&self) -> Self::NetworkId {
        1
    }

    fn subnetworks(&self) -> HashMap<Self::NetworkId, HashSet<Self::Id>> {
        HashMap::new()
    }

    fn session_id(&self) -> lb_core::sdp::SessionNumber {
        0
    }
}

impl AddressBookHandler for AllNeighbours {
    type Id = PeerId;

    fn get_address(&self, peer_id: &Self::Id) -> Option<libp2p::Multiaddr> {
        self.addresses.lock().unwrap().get(peer_id).cloned()
    }
}

pub fn new_swarm_in_memory<TBehavior>(
    key: &Keypair,
    behavior: TBehavior,
) -> libp2p::Swarm<TBehavior>
where
    TBehavior: NetworkBehaviour + Send,
{
    libp2p::SwarmBuilder::with_existing_identity(key.clone())
        .with_tokio()
        .with_other_transport(|_| {
            let transport = MemoryTransport::default()
                .upgrade(Version::V1)
                .authenticate(libp2p::plaintext::Config::new(key))
                .multiplex(libp2p::yamux::Config::default())
                .timeout(Duration::from_secs(20));

            Ok(transport)
        })
        .unwrap()
        .with_behaviour(|_| behavior)
        .unwrap()
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(20)))
        .build()
}
