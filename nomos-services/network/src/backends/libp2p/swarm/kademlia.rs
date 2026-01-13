use std::collections::{HashMap, HashSet};

use nomos_libp2p::{
    Multiaddr, PeerId, Protocol,
    libp2p::kad::{self, PeerInfo, ProgressStep, QueryId},
};
use rand::RngCore;
use tokio::sync::oneshot;

use crate::backends::libp2p::swarm::SwarmHandler;

#[derive(Debug)]
#[non_exhaustive]
pub enum DiscoveryCommand {
    GetClosestPeers {
        peer_id: PeerId,
        reply: oneshot::Sender<Vec<PeerInfo>>,
    },
    GetDiscoveredPeers {
        reply: oneshot::Sender<HashSet<PeerId>>,
    },
    DumpRoutingTable {
        reply: oneshot::Sender<HashMap<u32, Vec<PeerId>>>,
    },
}

// Define a struct to hold the data
pub struct PendingQueryData {
    sender: oneshot::Sender<Vec<PeerInfo>>,
    accumulated_results: Vec<PeerInfo>,
}

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    pub(super) fn bootstrap_kad_from_peers(&mut self, initial_peers: &Vec<Multiaddr>) {
        for peer_addr in initial_peers {
            if let Some(Protocol::P2p(peer_id_bytes)) = peer_addr.iter().last() {
                if let Ok(peer_id) = PeerId::from_multihash(peer_id_bytes.into()) {
                    self.swarm.kademlia_add_address(peer_id, peer_addr.clone());
                    tracing::debug!("Added peer to Kademlia: {} at {}", peer_id, peer_addr);
                } else {
                    tracing::warn!("Failed to parse peer ID from multiaddr: {}", peer_addr);
                }
            } else {
                tracing::warn!("Multiaddr doesn't contain peer ID: {}", peer_addr);
            }
        }
    }

    pub(super) fn handle_discovery_command(&mut self, command: DiscoveryCommand) {
        match command {
            DiscoveryCommand::GetClosestPeers { peer_id, reply } => {
                let query_id = self.swarm.get_closest_peers(peer_id);
                tracing::debug!("Pending query ID: {query_id}");
                self.pending_queries.insert(
                    query_id,
                    PendingQueryData {
                        sender: reply,
                        accumulated_results: Vec::new(),
                    },
                );
            }
            DiscoveryCommand::GetDiscoveredPeers { reply } => {
                let discovered_peers = self
                    .swarm
                    .kademlia_discovered_peers()
                    .into_iter()
                    .map(|peer_info| peer_info.peer_id)
                    .collect::<HashSet<_>>();

                drop(reply.send(discovered_peers));
            }
            DiscoveryCommand::DumpRoutingTable { reply } => {
                let result = self.swarm.kademlia_routing_table_dump();
                tracing::debug!("Routing table dump: {result:?}");
                drop(reply.send(result));
            }
        }
    }

    pub(super) fn handle_kademlia_event(&mut self, event: kad::Event) {
        match event {
            kad::Event::OutboundQueryProgressed {
                id, result, step, ..
            } => {
                self.handle_query_progress(id, result, &step);
            }
            kad::Event::RoutingUpdated {
                peer,
                addresses,
                old_peer,
                is_new_peer,
                ..
            } => {
                log_routing_update(peer, &addresses.into_vec(), old_peer, is_new_peer);
            }
            event => {
                tracing::debug!("Kademlia event: {:?}", event);
            }
        }
    }

    pub(super) fn handle_query_progress(
        &mut self,
        id: QueryId,
        result: kad::QueryResult,
        step: &ProgressStep,
    ) {
        match result {
            kad::QueryResult::GetClosestPeers(Ok(result)) => {
                if let Some(query_data) = self.pending_queries.get_mut(&id) {
                    query_data.accumulated_results.extend(result.peers);

                    if step.last
                        && let Some(query_data) = self.pending_queries.remove(&id)
                    {
                        drop(query_data.sender.send(query_data.accumulated_results));
                    }
                }
            }
            kad::QueryResult::GetClosestPeers(Err(err)) => {
                tracing::warn!("Failed to find closest peers: {:?}", err);
                // For errors, we should probably just send what we have so far
                if let Some(query_data) = self.pending_queries.remove(&id) {
                    drop(query_data.sender.send(query_data.accumulated_results));
                }
            }
            _ => {
                tracing::debug!("Handle kademlia query result: {:?}", result);
            }
        }
    }
}

fn log_routing_update(
    peer: PeerId,
    address: &[Multiaddr],
    old_peer: Option<PeerId>,
    is_new_peer: bool,
) {
    tracing::debug!(
        "Routing table updated: peer: {peer}, address: {address:?}, \
         old_peer: {old_peer:?}, is_new_peer: {is_new_peer}"
    );
}
