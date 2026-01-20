use std::collections::HashMap;

use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    kad::{PeerInfo, QueryId},
};
use rand::RngCore;

use crate::behaviour::Behaviour;

impl<R: Clone + Send + RngCore + 'static> Behaviour<R> {
    pub(crate) fn kademlia_add_address(&mut self, peer_id: PeerId, addr: Multiaddr) {
        self.kademlia.add_address(&peer_id, addr);
    }

    pub(crate) fn kademlia_routing_table_dump(&mut self) -> HashMap<u32, Vec<PeerId>> {
        self.kademlia
            .kbuckets()
            .enumerate()
            .map(|(bucket_idx, bucket)| {
                let peers = bucket
                    .iter()
                    .map(|entry| *entry.node.key.preimage())
                    .collect::<Vec<_>>();
                let bucket_idx: u32 = bucket_idx.try_into().expect("Bucket index to be u32 MAX.");
                (bucket_idx, peers)
            })
            .collect()
    }

    pub(crate) fn get_kademlia_protocol_names(&self) -> impl Iterator<Item = &StreamProtocol> {
        self.kademlia.protocol_names().iter()
    }

    pub(crate) fn kademlia_get_closest_peers(&mut self, peer_id: PeerId) -> QueryId {
        self.kademlia.get_closest_peers(peer_id)
    }

    pub(crate) fn kademlia_discovered_peers(&mut self) -> Vec<PeerInfo> {
        // get all buckets and in each buket, peers with addresses
        self.kademlia
            .kbuckets()
            .flat_map(|bucket| {
                bucket
                    .iter()
                    .filter_map(|entry| {
                        let peer_id = *entry.node.key.preimage();
                        let addresses: Vec<_> = entry.node.value.iter().cloned().collect();
                        (!addresses.is_empty()).then_some(PeerInfo {
                            peer_id,
                            addrs: addresses,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}
