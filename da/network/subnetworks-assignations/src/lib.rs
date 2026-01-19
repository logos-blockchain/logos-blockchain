pub mod versions;

use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    hash::Hash,
    sync::Arc,
};

use lb_core::sdp::SessionNumber;
use rand::RngCore;

pub type SubnetworkId = u16;

pub type SubnetworkAssignations<NetworkId, Id> = HashMap<NetworkId, HashSet<Id>>;

pub trait MembershipCreator: MembershipHandler {
    /// Initializes the underlying implementor with the provided members list.
    #[must_use]
    fn init(
        &self,
        session_id: SessionNumber,
        peer_addresses: SubnetworkAssignations<Self::NetworkId, Self::Id>,
    ) -> Self;

    /// Creates a new instance of membership handler that combines previous
    /// members and new members.
    #[must_use]
    fn update<Rng: RngCore>(
        &self,
        session_id: SessionNumber,
        new_peers: HashSet<Self::Id>,
        rng: &mut Rng,
    ) -> Self;
}

pub trait MembershipHandler {
    /// Subnetworks Id type
    type NetworkId: Eq + Debug + Hash;
    /// Members Id type
    type Id: Debug;

    fn session_id(&self) -> SessionNumber;

    /// Returns the set of `NetworksIds` an id is a member of
    fn membership(&self, id: &Self::Id) -> HashSet<Self::NetworkId>;

    /// True if the id is a member of a `network_id`, False otherwise
    fn is_member_of(&self, id: &Self::Id, network_id: &Self::NetworkId) -> bool {
        self.membership(id).contains(network_id)
    }

    /// Returns true if the member id is in the overall membership set
    fn is_allowed(&self, id: &Self::Id) -> bool;

    /// Returns the set of members in a subnetwork by its `NetworkId`
    fn members_of(&self, network_id: &Self::NetworkId) -> HashSet<Self::Id>;

    /// Returns the set of all members
    fn members(&self) -> HashSet<Self::Id>;

    fn last_subnetwork_id(&self) -> Self::NetworkId;

    /// Returns all subnetworks with assigned members.
    fn subnetworks(&self) -> SubnetworkAssignations<Self::NetworkId, Self::Id>;
}

impl<T> MembershipHandler for Arc<T>
where
    T: MembershipHandler,
{
    type NetworkId = T::NetworkId;
    type Id = T::Id;

    fn membership(&self, id: &Self::Id) -> HashSet<Self::NetworkId> {
        self.as_ref().membership(id)
    }

    fn is_allowed(&self, id: &Self::Id) -> bool {
        self.as_ref().is_allowed(id)
    }

    fn members_of(&self, network_id: &Self::NetworkId) -> HashSet<Self::Id> {
        self.as_ref().members_of(network_id)
    }

    fn members(&self) -> HashSet<Self::Id> {
        self.as_ref().members()
    }

    fn last_subnetwork_id(&self) -> Self::NetworkId {
        self.as_ref().last_subnetwork_id()
    }

    fn subnetworks(&self) -> HashMap<Self::NetworkId, HashSet<Self::Id>> {
        self.as_ref().subnetworks()
    }

    fn session_id(&self) -> SessionNumber {
        self.as_ref().session_id()
    }
}
