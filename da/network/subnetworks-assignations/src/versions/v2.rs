use std::collections::HashSet;

use lb_core::sdp::SessionNumber;
use libp2p_identity::PeerId;
use serde::{Deserialize, Serialize};

use crate::{MembershipHandler, SubnetworkAssignations};

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct FillWithOriginalReplication {
    assignations: Vec<HashSet<PeerId>>,
    subnetwork_size: usize,
    dispersal_factor: usize,
    original_replication: usize,
    pivot: u16,
    session_id: SessionNumber,
}

impl FillWithOriginalReplication {
    #[must_use]
    pub fn new(
        session_id: SessionNumber,
        peers: &[PeerId],
        subnetwork_size: usize,
        dispersal_factor: usize,
        original_replication: usize,
        pivot: u16,
    ) -> Self {
        Self {
            assignations: Self::fill(
                peers,
                subnetwork_size,
                dispersal_factor,
                original_replication,
                pivot,
            ),
            subnetwork_size,
            dispersal_factor,
            original_replication,
            pivot,
            session_id,
        }
    }
    fn fill(
        peers: &[PeerId],
        subnetwork_size: usize,
        dispersal_factor: usize,
        original_replication: usize,
        pivot: u16,
    ) -> Vec<HashSet<PeerId>> {
        assert!(!peers.is_empty());
        // sort list to make it deterministic
        let mut peers = peers.to_vec();
        peers.sort_unstable();
        // take n peers and fill a subnetwork until all subnetworks are filled
        let mut cycle = peers.into_iter().cycle();
        (0..subnetwork_size)
            .map(|subnetwork| {
                std::iter::repeat_with(|| cycle.next().unwrap())
                    .take({
                        // choose factor depending on if it is in the original size of the encoding
                        // or not
                        if subnetwork < pivot as usize {
                            original_replication
                        } else {
                            dispersal_factor
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

impl MembershipHandler for FillWithOriginalReplication {
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

    use crate::versions::v2::FillWithOriginalReplication;

    #[test]
    fn test_distribution_fill_with_original_replication_from_node_list() {
        let nodes: Vec<_> = std::iter::repeat_with(PeerId::random).take(100).collect();
        let dispersal_factor = 2;
        let subnetwork_size = 1024;
        let original_replication = 10;
        let pivot = 512;
        let distribution = FillWithOriginalReplication::new(
            0,
            &nodes,
            subnetwork_size,
            dispersal_factor,
            original_replication,
            pivot,
        );
        assert_eq!(distribution.assignations.len(), subnetwork_size);
        for subnetwork in &distribution.assignations[..pivot as usize] {
            assert_eq!(subnetwork.len(), original_replication);
        }
        for subnetwork in &distribution.assignations[pivot as usize..] {
            assert_eq!(subnetwork.len(), dispersal_factor);
        }
    }
}
