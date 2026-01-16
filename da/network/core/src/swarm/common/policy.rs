use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use subnetworks_assignations::MembershipHandler;

use super::balancer::{
    ConnectionDeviation, SubnetworkConnectionPolicy, SubnetworkDeviation, SubnetworkStats,
};
use crate::{
    SubnetworkId,
    swarm::common::monitor::{PeerHealthPolicy, PeerStats},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DAConnectionPolicySettings {
    pub min_dispersal_peers: usize,
    pub min_replication_peers: usize,
    pub max_dispersal_failures: usize,
    pub max_sampling_failures: usize,
    pub max_replication_failures: usize,
    pub malicious_threshold: usize,
}

#[derive(Clone)]
pub struct DAConnectionPolicy<Membership>
where
    Membership: Clone,
{
    settings: DAConnectionPolicySettings,
    membership: Membership,
    local_peer_id: PeerId,
}

impl<Membership> DAConnectionPolicy<Membership>
where
    Membership: Clone,
{
    pub const fn new(
        settings: DAConnectionPolicySettings,
        membership: Membership,
        local_peer_id: PeerId,
    ) -> Self {
        Self {
            settings,
            membership,
            local_peer_id,
        }
    }
}

impl<Membership> PeerHealthPolicy for DAConnectionPolicy<Membership>
where
    Membership: Clone,
{
    type PeerStats = PeerStats;

    fn is_peer_malicious(&self, stats: &Self::PeerStats) -> bool {
        stats.dispersal_failures_rate >= self.settings.malicious_threshold
            || stats.sampling_failures_rate >= self.settings.malicious_threshold
            || stats.replication_failures_rate >= self.settings.malicious_threshold
    }

    fn is_peer_unhealthy(&self, stats: &Self::PeerStats) -> bool {
        stats.dispersal_failures_rate >= self.settings.max_dispersal_failures
            || stats.sampling_failures_rate >= self.settings.max_sampling_failures
            || stats.replication_failures_rate >= self.settings.max_replication_failures
    }
}

impl<Membership> SubnetworkConnectionPolicy for DAConnectionPolicy<Membership>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId> + Clone,
{
    fn connection_number_deviation(
        &self,
        subnetwork_id: SubnetworkId,
        stats: &SubnetworkStats,
    ) -> SubnetworkDeviation {
        let is_member = self
            .membership
            .is_member_of(&self.local_peer_id, &subnetwork_id);

        let required_connections = if is_member {
            self.settings
                .min_dispersal_peers
                .max(self.settings.min_replication_peers)
        } else {
            self.settings.min_dispersal_peers
        };

        // Current implementation doesn't differenciate and combine inbound and outbound
        // stats.
        let total_missing = required_connections.saturating_sub(stats.inbound + stats.outbound);

        SubnetworkDeviation {
            // All missing are counted as outbound.
            outbound: ConnectionDeviation::Missing(total_missing),
        }
    }
}
