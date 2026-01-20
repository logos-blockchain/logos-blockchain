use std::collections::HashMap;

use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    kad::{PeerInfo, QueryId},
};
use rand::RngCore;

use crate::Swarm;

impl<R: Clone + Send + RngCore + 'static> Swarm<R> {
    pub fn get_closest_peers(&mut self, peer_id: PeerId) -> QueryId {
        self.swarm
            .behaviour_mut()
            .kademlia_get_closest_peers(peer_id)
    }

    pub fn get_kademlia_protocol_names(&self) -> impl Iterator<Item = &StreamProtocol> {
        self.swarm.behaviour().get_kademlia_protocol_names()
    }

    pub fn kademlia_add_address(&mut self, peer_id: PeerId, addr: Multiaddr) {
        self.swarm
            .behaviour_mut()
            .kademlia_add_address(peer_id, addr);
    }

    pub fn kademlia_routing_table_dump(&mut self) -> HashMap<u32, Vec<PeerId>> {
        self.swarm.behaviour_mut().kademlia_routing_table_dump()
    }

    pub fn kademlia_discovered_peers(&mut self) -> Vec<PeerInfo> {
        self.swarm.behaviour_mut().kademlia_discovered_peers()
    }
}
