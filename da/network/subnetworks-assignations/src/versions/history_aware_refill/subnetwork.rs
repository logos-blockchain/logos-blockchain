use std::{cmp::Ordering, collections::BTreeSet};

use crate::SubnetworkId;

#[derive(Eq)]
pub(super) struct Subnetwork<Id> {
    pub participants: BTreeSet<Id>,
    pub subnetwork_id: SubnetworkId,
}

impl<Id: PartialEq> PartialEq for Subnetwork<Id> {
    fn eq(&self, other: &Self) -> bool {
        self.subnetwork_id == other.subnetwork_id
    }
}

impl<Id: Ord> Ord for Subnetwork<Id> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap()
    }
}
impl<Id: PartialOrd> PartialOrd for Subnetwork<Id> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(
            (self.participants.len(), self.subnetwork_id)
                .cmp(&(other.participants.len(), other.subnetwork_id)),
        )
    }
}

impl<Id> Subnetwork<Id> {
    pub fn len(&self) -> usize {
        self.participants.len()
    }
}
