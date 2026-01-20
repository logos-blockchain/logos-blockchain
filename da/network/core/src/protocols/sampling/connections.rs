use std::collections::{HashMap, HashSet};

use libp2p::PeerId;

use crate::SubnetworkId;

type RetryCount = usize;

pub struct Connections {
    pending_connections: HashMap<SubnetworkId, RetryCount>,
    pending_peers: HashMap<PeerId, HashSet<SubnetworkId>>,
    failed_peers: HashSet<PeerId>,
    retry_limit: usize,
}

impl Connections {
    pub fn new(retry_limit: usize) -> Self {
        Self {
            pending_connections: HashMap::new(),
            pending_peers: HashMap::new(),
            failed_peers: HashSet::new(),
            retry_limit,
        }
    }

    pub fn register_connect(&mut self, peer: PeerId) {
        self.failed_peers.remove(&peer);
        if let Some(subnets) = self.pending_peers.get(&peer) {
            for subnet in subnets {
                self.pending_connections.insert(*subnet, 0);
            }
        }
    }

    pub fn register_disconnect(&mut self, peer: PeerId) {
        if let Some(subnets) = self.pending_peers.get(&peer) {
            self.failed_peers.insert(peer);
            for subnet in subnets {
                self.pending_connections
                    .entry(*subnet)
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }
        }
    }

    pub fn register_pending_peer(&mut self, peer: PeerId, subnet: SubnetworkId) {
        self.pending_connections.insert(subnet, 0);
        self.pending_peers.entry(peer).or_default().insert(subnet);
    }

    pub fn should_retry(&self, subnet: SubnetworkId) -> bool {
        // If we don't track the subnetwork yet, we should try to connect to it.
        self.pending_connections
            .get(&subnet)
            .is_none_or(|&retries| retries < self.retry_limit)
    }

    pub fn should_requeue(&self, peer: PeerId) -> bool {
        self.failed_peers.contains(&peer)
    }

    pub fn clear(&mut self) {
        self.pending_connections.clear();
        self.pending_peers.clear();
        self.failed_peers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_limits() {
        let retry_limit = 2;
        let mut connections = Connections::new(retry_limit);

        let peer = PeerId::random();
        let subnet = 1;

        connections.register_pending_peer(peer, subnet);
        assert_eq!(connections.pending_connections.get(&subnet), Some(&0));
        assert!(connections.should_retry(subnet));

        connections.register_disconnect(peer);
        assert_eq!(connections.pending_connections.get(&subnet), Some(&1));
        assert!(connections.should_retry(subnet));

        connections.register_connect(peer);
        assert_eq!(connections.pending_connections.get(&subnet), Some(&0));
        assert!(!connections.should_requeue(peer));
        assert!(connections.should_retry(subnet));

        connections.register_disconnect(peer);
        assert!(connections.should_requeue(peer));
        connections.register_disconnect(peer);
        connections.register_disconnect(peer);

        assert_eq!(connections.pending_connections.get(&subnet), Some(&3));
        assert!(!connections.should_retry(subnet));

        connections.clear();
        assert_eq!(connections.pending_connections.len(), 0);
        assert_eq!(connections.pending_peers.len(), 0);
        assert!(connections.should_retry(subnet));
    }
}
