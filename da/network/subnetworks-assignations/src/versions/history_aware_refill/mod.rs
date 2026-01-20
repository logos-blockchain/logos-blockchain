use std::{
    collections::{BTreeSet, HashSet},
    fmt::Debug,
    hash::Hash,
};

use lb_core::sdp::SessionNumber;
use rand::RngCore;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    MembershipCreator, MembershipHandler, SubnetworkAssignations, SubnetworkId,
    versions::history_aware_refill::assignations::HistoryAwareRefill,
};

pub mod assignations;
mod participant;
mod subnetwork;

#[derive(Clone, Debug, Serialize)]
pub struct HistoryAware<Id>
where
    Id: Ord,
{
    #[serde(skip)]
    assignations: assignations::Assignations<Id>,
    subnetwork_size: usize,
    replication_factor: usize,
    session_id: SessionNumber,
}

impl<Id: Ord> HistoryAware<Id> {
    pub fn new(
        session_id: SessionNumber,
        subnetwork_size: usize,
        replication_factor: usize,
    ) -> Self {
        Self {
            assignations: std::iter::repeat_with(BTreeSet::new)
                .take(subnetwork_size)
                .collect(),
            subnetwork_size,
            replication_factor,
            session_id,
        }
    }
}

impl<'de, Id> Deserialize<'de> for HistoryAware<Id>
where
    Id: Ord,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct HistoryAwareHelper {
            subnetwork_size: usize,
            replication_factor: usize,
            #[serde(default)]
            session_id: SessionNumber,
        }

        let helper = HistoryAwareHelper::deserialize(deserializer)?;

        Ok(Self::new(
            helper.session_id,
            helper.subnetwork_size,
            helper.replication_factor,
        ))
    }
}

impl<Id> MembershipHandler for HistoryAware<Id>
where
    Id: Ord + PartialOrd + Eq + Copy + Hash + Debug,
{
    type NetworkId = SubnetworkId;
    type Id = Id;

    fn membership(&self, id: &Self::Id) -> HashSet<Self::NetworkId> {
        self.assignations
            .iter()
            .enumerate()
            .filter_map(|(subnetwork_id, subnetwork)| {
                subnetwork
                    .contains(id)
                    .then_some(subnetwork_id as Self::NetworkId)
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
        self.assignations
            .get(*network_id as usize)
            .map_or_else(HashSet::new, |subnetwork| {
                subnetwork.iter().copied().collect()
            })
    }

    fn members(&self) -> HashSet<Self::Id> {
        self.assignations.iter().flatten().copied().collect()
    }

    fn last_subnetwork_id(&self) -> Self::NetworkId {
        self.assignations.len() as Self::NetworkId
    }

    fn subnetworks(&self) -> SubnetworkAssignations<Self::NetworkId, Self::Id> {
        self.assignations
            .iter()
            .enumerate()
            .map(|(id, subnetwork)| {
                (
                    id as Self::NetworkId,
                    subnetwork.iter().copied().collect::<HashSet<_>>(),
                )
            })
            .collect()
    }

    fn session_id(&self) -> SessionNumber {
        self.session_id
    }
}

impl<Id> MembershipCreator for HistoryAware<Id>
where
    for<'id> Id: Ord + PartialOrd + Eq + Copy + Hash + Debug + 'id,
{
    fn init(
        &self,
        session_id: SessionNumber,
        peer_addresses: SubnetworkAssignations<Self::NetworkId, Self::Id>,
    ) -> Self {
        let mut assignations: Vec<_> = peer_addresses.into_iter().collect();
        assignations.sort_by_key(|(id, _)| *id);
        let assignations: Vec<_> = assignations
            .into_iter()
            .map(|(_, ids)| ids.into_iter().collect())
            .collect();
        Self {
            assignations,
            subnetwork_size: self.subnetwork_size,
            replication_factor: self.replication_factor,
            session_id,
        }
    }

    fn update<Rng: RngCore>(
        &self,
        session_id: SessionNumber,
        new_nodes: HashSet<Self::Id>,
        rng: &mut Rng,
    ) -> Self {
        let Self {
            assignations,
            subnetwork_size,
            replication_factor,
            ..
        } = self.clone();
        let new_nodes: Vec<_> = new_nodes.into_iter().collect();

        let assignations = HistoryAwareRefill::calculate_subnetwork_assignations(
            &new_nodes,
            assignations,
            replication_factor,
            rng,
        );
        Self {
            assignations,
            subnetwork_size,
            replication_factor,
            session_id,
        }
    }
}
