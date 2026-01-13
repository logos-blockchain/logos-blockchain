use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    task::{Context, Poll},
};

use libp2p::PeerId;
use rand::seq::IteratorRandom as _;
use serde::{Deserialize, Serialize};
use subnetworks_assignations::MembershipHandler;

use crate::{
    SubnetworkId,
    maintenance::balancer::{ConnectionBalancer, ConnectionEvent},
};

pub type BalancerStats = HashMap<SubnetworkId, SubnetworkStats>;

#[derive(Copy, Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubnetworkStats {
    pub inbound: usize,
    pub outbound: usize,
}

pub enum ConnectionDeviation {
    // Since DA protocol at the moment expects any number of nodes to be able to sample at any
    // time, are only checking for insufficient number of connections.
    Missing(usize),
}

pub struct SubnetworkDeviation {
    // Missing inbound is for now ignored as we have no way to request peers to connect
    // to us.
    pub outbound: ConnectionDeviation,
}

pub trait SubnetworkConnectionPolicy {
    fn connection_number_deviation(
        &self,
        subnetwork_id: SubnetworkId,
        stats: &SubnetworkStats,
    ) -> SubnetworkDeviation;
}

#[derive(Clone, Copy)]
enum ConnectionDirection {
    Inbound,
    Outbound,
}

pub struct DAConnectionBalancer<Membership, Policy> {
    local_peer_id: PeerId,
    membership: Membership,
    policy: Policy,
    interval: Pin<Box<dyn futures::Stream<Item = ()> + Send>>,
    subnetwork_stats: BalancerStats,
    connected_peers: HashMap<PeerId, ConnectionDirection>,
}

impl<Membership, Policy> DAConnectionBalancer<Membership, Policy>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>,
    Policy: SubnetworkConnectionPolicy,
{
    pub fn new(
        local_peer_id: PeerId,
        membership: Membership,
        policy: Policy,
        interval: impl futures::Stream<Item = ()> + Send + 'static,
    ) -> Self {
        Self {
            local_peer_id,
            membership,
            policy,
            interval: Box::pin(interval),
            subnetwork_stats: HashMap::new(),
            connected_peers: HashMap::new(),
        }
    }

    fn update_subnetwork_stats(
        &mut self,
        subnetwork_id: SubnetworkId,
        inbound: isize,
        outbound: isize,
    ) {
        let stats = self
            .subnetwork_stats
            .entry(subnetwork_id)
            .or_insert(SubnetworkStats {
                inbound: 0,
                outbound: 0,
            });

        stats.inbound = (stats.inbound as isize + inbound).max(0) as usize;
        stats.outbound = (stats.outbound as isize + outbound).max(0) as usize;
    }

    fn select_missing_peers(
        &self,
        subnetwork_id: SubnetworkId,
        missing_count: usize,
    ) -> Vec<PeerId> {
        let candidates = self.membership.members_of(&subnetwork_id);
        candidates
            .into_iter()
            .filter(|peer| !self.connected_peers.contains_key(peer) && *peer != self.local_peer_id)
            .choose_multiple(&mut rand::rng(), missing_count)
    }
}

impl<Membership, Policy> ConnectionBalancer for DAConnectionBalancer<Membership, Policy>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>,
    Policy: SubnetworkConnectionPolicy,
{
    type Stats = BalancerStats;

    fn record_event(&mut self, event: ConnectionEvent) {
        match event {
            ConnectionEvent::OpenInbound(peer) => {
                self.connected_peers
                    .insert(peer, ConnectionDirection::Inbound);
                let subnets: Vec<_> = self.membership.membership(&peer).into_iter().collect();
                tracing::debug!("BALANCER: OpenInbound peer={peer}, subnets={subnets:?}");

                for subnetwork in self.membership.membership(&peer) {
                    self.update_subnetwork_stats(subnetwork, 1, 0);
                }
            }
            ConnectionEvent::OpenOutbound(peer) => {
                self.connected_peers
                    .insert(peer, ConnectionDirection::Outbound);
                let subnets: Vec<_> = self.membership.membership(&peer).into_iter().collect();
                tracing::debug!("BALANCER: OpenOutbound peer={peer}, subnets={subnets:?}");

                for subnetwork in self.membership.membership(&peer) {
                    self.update_subnetwork_stats(subnetwork, 0, 1);
                }
            }
            ConnectionEvent::CloseInbound(peer) => {
                self.connected_peers.remove(&peer);
                for subnetwork in self.membership.membership(&peer) {
                    self.update_subnetwork_stats(subnetwork, -1, 0);
                }
            }
            ConnectionEvent::CloseOutbound(peer) => {
                self.connected_peers.remove(&peer);
                for subnetwork in self.membership.membership(&peer) {
                    self.update_subnetwork_stats(subnetwork, 0, -1);
                }
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<VecDeque<PeerId>> {
        if self.interval.as_mut().poll_next(cx).is_ready() {
            let mut peers_to_connect = VecDeque::new();

            for subnetwork_id in 0..=self.membership.last_subnetwork_id() {
                let stats = self
                    .subnetwork_stats
                    .get(&subnetwork_id)
                    .unwrap_or(&SubnetworkStats {
                        inbound: 0,
                        outbound: 0,
                    });
                let deviation = self
                    .policy
                    .connection_number_deviation(subnetwork_id, stats);

                // In DA balancer implementation we are only concerned about missing peer
                // connections. Sampling protocol requires to allow any number of peers to
                // request for a sample, which requires to allow any number of
                // peers to connect at any given time.
                let ConnectionDeviation::Missing(missing_count) = deviation.outbound;
                peers_to_connect.extend(self.select_missing_peers(subnetwork_id, missing_count));
            }

            if peers_to_connect.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(peers_to_connect)
            }
        } else {
            Poll::Pending
        }
    }

    fn refresh_subnet_attributions(&mut self) {
        tracing::info!(
            "BALANCER: Refreshing subnet attributions for {} connected peers",
            self.connected_peers.len()
        );
        self.subnetwork_stats.clear();

        // todo: this is a quick fix, rethink to support large connected peers and
        // membership changes
        let connected_peers: Vec<_> = self
            .connected_peers
            .iter()
            .map(|(peer_id, direction)| (*peer_id, *direction))
            .collect();

        for (peer_id, direction) in &connected_peers {
            let subnets: Vec<_> = self.membership.membership(peer_id).into_iter().collect();
            tracing::debug!("BALANCER: Re-attributing peer={peer_id}, subnets={subnets:?}");
            for subnetwork_id in subnets {
                match direction {
                    ConnectionDirection::Inbound => {
                        self.update_subnetwork_stats(subnetwork_id, 1, 0);
                    }
                    ConnectionDirection::Outbound => {
                        self.update_subnetwork_stats(subnetwork_id, 0, 1);
                    }
                }
            }
        }

        tracing::debug!(
            "BALANCER: Subnet stats after refresh: {:?}",
            self.subnetwork_stats
        );
    }

    fn stats(&self) -> Self::Stats {
        self.subnetwork_stats.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use futures::stream;
    use tokio_stream::StreamExt as _;

    use super::*;

    struct MockPolicy {
        missing: usize,
    }

    impl SubnetworkConnectionPolicy for MockPolicy {
        fn connection_number_deviation(
            &self,
            _id: SubnetworkId,
            _stats: &SubnetworkStats,
        ) -> SubnetworkDeviation {
            SubnetworkDeviation {
                outbound: ConnectionDeviation::Missing(self.missing),
            }
        }
    }

    struct MockMembership {
        subnetwork: SubnetworkId,
        members: HashSet<PeerId>,
        last_subnet_id: usize,
    }

    impl MembershipHandler for MockMembership {
        type NetworkId = SubnetworkId;
        type Id = PeerId;

        fn membership(&self, id: &Self::Id) -> HashSet<Self::NetworkId> {
            if self.members.contains(id) {
                HashSet::from([self.subnetwork])
            } else {
                HashSet::new()
            }
        }

        fn is_allowed(&self, id: &Self::Id) -> bool {
            self.members.contains(id)
        }

        fn members_of(&self, network_id: &Self::NetworkId) -> HashSet<Self::Id> {
            if *network_id == self.subnetwork {
                self.members.clone()
            } else {
                HashSet::new()
            }
        }

        fn members(&self) -> HashSet<Self::Id> {
            self.members.clone()
        }

        fn last_subnetwork_id(&self) -> Self::NetworkId {
            self.last_subnet_id as u16
        }

        fn subnetworks(&self) -> HashMap<Self::NetworkId, HashSet<Self::Id>> {
            unimplemented!()
        }

        fn session_id(&self) -> nomos_core::sdp::SessionNumber {
            0
        }
    }

    #[tokio::test]
    async fn test_balancer_returns_one_peer() {
        let subnetwork_id = SubnetworkId::default();
        let peer1 = PeerId::random();
        let peer2 = PeerId::random();

        let membership = MockMembership {
            subnetwork: subnetwork_id,
            members: HashSet::from([peer1, peer2]),
            last_subnet_id: 0,
        };

        let policy = MockPolicy { missing: 1 };

        let interval = stream::once(async {}).chain(stream::pending());
        let mut balancer = DAConnectionBalancer::new(peer1, membership, policy, interval);

        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let poll_result = balancer.poll(&mut cx);

        assert!(matches!(poll_result, Poll::Ready(ref peers) if peers.len() == 1));
        let Poll::Ready(peers) = poll_result else {
            panic!("Expected Poll::Ready with peers")
        };

        assert_eq!(peers.len(), 1);
        assert!(peers.contains(&peer2));
    }

    #[tokio::test]
    async fn test_balancer_returns_multiple_peers() {
        let subnetwork_id = SubnetworkId::default();
        let peer1 = PeerId::random();
        let peer2 = PeerId::random();
        let peer3 = PeerId::random();
        let peer4 = PeerId::random();

        let membership = MockMembership {
            subnetwork: subnetwork_id,
            members: HashSet::from([peer1, peer2, peer3, peer4]),
            last_subnet_id: 0,
        };

        let policy = MockPolicy { missing: 2 };

        let interval = stream::once(async {}).chain(stream::pending());
        let mut balancer = DAConnectionBalancer::new(peer1, membership, policy, interval);

        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let poll_result = balancer.poll(&mut cx);

        assert!(matches!(poll_result, Poll::Ready(ref peers) if peers.len() == 2));
        let Poll::Ready(peers) = poll_result else {
            panic!("Expected Poll::Ready with peers")
        };

        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&peer2) || peers.contains(&peer3) || peers.contains(&peer4));
    }

    #[tokio::test]
    async fn test_balancer_returns_pending_if_no_peers_needed() {
        let subnetwork_id = SubnetworkId::default();
        let peer1 = PeerId::random();
        let peer2 = PeerId::random();

        let membership = MockMembership {
            subnetwork: subnetwork_id,
            members: HashSet::from([peer1, peer2]),
            last_subnet_id: 0,
        };

        let policy = MockPolicy { missing: 0 };

        let interval = stream::once(async {}).chain(stream::pending());
        let mut balancer = DAConnectionBalancer::new(peer1, membership, policy, interval);

        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let poll_result = balancer.poll(&mut cx);

        assert!(matches!(poll_result, Poll::Pending));
    }
}
