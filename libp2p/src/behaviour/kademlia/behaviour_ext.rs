use std::collections::HashMap;

use lb_log_targets::libp2p as lb_log_targets_libp2p;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    kad::{PeerInfo, QueryId, RoutingUpdate},
};
use rand::RngCore;

use crate::behaviour::Behaviour;

const LOG_TARGET: &str = lb_log_targets_libp2p::behaviour::KADEMLIA;

impl<R: Clone + Send + RngCore + 'static> Behaviour<R> {
    pub(crate) fn kademlia_add_address(&mut self, peer_id: PeerId, addr: &Multiaddr) {
        match self.kademlia.add_address(&peer_id, addr.clone()) {
            RoutingUpdate::Success => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "Added address {:?} to peer {:?}",
                    addr,
                    peer_id
                );
            }
            RoutingUpdate::Pending => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "Pending to add address {:?} to peer {:?}",
                    addr,
                    peer_id
                );
            }
            RoutingUpdate::Failed => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "Failed to add address {:?} to peer {:?}",
                    addr,
                    peer_id
                );
            }
        }
    }

    /// Removes `addr` of `peer_id` from Kademlia, even if it is the last
    /// address of the peer.
    pub(crate) fn kademlia_remove_address(&mut self, peer_id: PeerId, addr: &Multiaddr) {
        if self.kademlia.remove_address(&peer_id, addr).is_some() {
            tracing::warn!(
                target: LOG_TARGET,
                "Removed peer {:?} from Kademlia with its last address {:?}",
                peer_id,
                addr
            );
        } else {
            tracing::warn!(
                target: LOG_TARGET,
                "Removed address {:?} of peer {:?} from Kademlia, if it was present",
                addr,
                peer_id
            );
        }
    }

    /// Returns whether `addr` is an address of `peer_id` in the routing table.
    pub(crate) fn kademlia_contains_address(&mut self, peer_id: PeerId, addr: &Multiaddr) -> bool {
        self.kademlia.kbucket(peer_id).is_some_and(|bucket| {
            bucket.iter().any(|entry| {
                *entry.node.key.preimage() == peer_id && entry.node.value.iter().any(|a| a == addr)
            })
        })
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
