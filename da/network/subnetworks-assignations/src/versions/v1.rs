use std::collections::HashSet;

use lb_core::sdp::SessionNumber;
use libp2p_identity::PeerId;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::{MembershipCreator, MembershipHandler, SubnetworkAssignations};

/// Fill a `N` sized set of "subnetworks" from a list of peer ids members
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct FillFromNodeList {
    assignations: Vec<HashSet<PeerId>>,
    subnetwork_size: usize,
    dispersal_factor: usize,
    session_id: SessionNumber,
}

impl FillFromNodeList {
    #[must_use]
    pub fn new(
        session_id: SessionNumber,
        peers: &[PeerId],
        subnetwork_size: usize,
        dispersal_factor: usize,
    ) -> Self {
        Self {
            assignations: Self::fill(peers, subnetwork_size, dispersal_factor),
            subnetwork_size,
            dispersal_factor,
            session_id,
        }
    }

    fn fill(
        peers: &[PeerId],
        subnetwork_size: usize,
        replication_factor: usize,
    ) -> Vec<HashSet<PeerId>> {
        if peers.is_empty() {
            return vec![HashSet::new(); subnetwork_size];
        }

        // sort list to make it deterministic
        let mut peers = peers.to_vec();
        peers.sort_unstable();
        // take n peers and fill a subnetwork until all subnetworks are filled
        let mut cycle = peers.into_iter().cycle();
        std::iter::repeat_with(|| {
            std::iter::repeat_with(|| cycle.next().unwrap())
                .take(replication_factor)
                .collect()
        })
        .take(subnetwork_size)
        .collect()
    }
}

impl MembershipCreator for FillFromNodeList {
    fn init(
        &self,
        session_id: SessionNumber,
        peer_addresses: SubnetworkAssignations<Self::NetworkId, PeerId>,
    ) -> Self {
        let members: Vec<Self::Id> = peer_addresses
            .values()
            .flat_map(|peer_set| peer_set.iter())
            .copied()
            .collect();

        Self {
            assignations: Self::fill(&members, self.subnetwork_size, self.dispersal_factor),
            subnetwork_size: self.subnetwork_size,
            dispersal_factor: self.dispersal_factor,
            session_id,
        }
    }

    fn update<Rng: RngCore>(
        &self,
        session_id: SessionNumber,
        new_peer_addresses: HashSet<Self::Id>,
        _: &mut Rng,
    ) -> Self {
        // todo: implement incremental update
        let members: Vec<Self::Id> = new_peer_addresses.into_iter().collect();

        Self {
            assignations: Self::fill(&members, self.subnetwork_size, self.dispersal_factor),
            subnetwork_size: self.subnetwork_size,
            dispersal_factor: self.dispersal_factor,
            session_id,
        }
    }
}

impl MembershipHandler for FillFromNodeList {
    type NetworkId = u16;
    type Id = PeerId;

    fn membership(&self, id: &Self::Id) -> HashSet<Self::NetworkId> {
        self.assignations
            .iter()
            .enumerate()
            .filter_map(|(network_id, subnetwork)| {
                subnetwork
                    .contains(id)
                    .then_some(network_id as Self::NetworkId)
            })
            .collect()
    }

    fn is_allowed(&self, id: &Self::Id) -> bool {
        for subnetwork in &self.assignations {
            if subnetwork.contains(id) {
                return true;
            }
        }
        false
    }

    fn members_of(&self, network_id: &Self::NetworkId) -> HashSet<Self::Id> {
        self.assignations[*network_id as usize].clone()
    }

    fn members(&self) -> HashSet<Self::Id> {
        self.assignations.iter().flatten().copied().collect()
    }

    fn last_subnetwork_id(&self) -> Self::NetworkId {
        self.subnetwork_size.saturating_sub(1) as u16
    }

    fn subnetworks(&self) -> SubnetworkAssignations<Self::NetworkId, Self::Id> {
        self.assignations
            .iter()
            .enumerate()
            .map(|(index, member_set)| {
                let network_id = index as Self::NetworkId;
                (network_id, member_set.clone())
            })
            .collect()
    }

    fn session_id(&self) -> SessionNumber {
        self.session_id
    }
}

#[cfg(test)]
mod test {
    use libp2p_identity::PeerId;

    use crate::versions::v1::FillFromNodeList;

    #[test]
    fn test_distribution_fill_from_node_list() {
        let nodes: Vec<_> = std::iter::repeat_with(PeerId::random).take(100).collect();
        let dispersal_factor = 2;
        let subnetwork_size = 1024;
        let distribution = FillFromNodeList::new(0, &nodes, subnetwork_size, dispersal_factor);
        assert_eq!(distribution.assignations.len(), subnetwork_size);
        for subnetwork in &distribution.assignations {
            assert_eq!(subnetwork.len(), dispersal_factor);
        }
    }
}
