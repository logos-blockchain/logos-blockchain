pub mod adapters;
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
};

use blake2::{Blake2b512, Digest as _, digest::Update as BlakeUpdate};
use multiaddr::Multiaddr;
use nomos_core::sdp::{ProviderId, SessionNumber};
use nomos_utils::blake_rng::BlakeRng;
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use rand::SeedableRng as _;
use subnetworks_assignations::{MembershipCreator, MembershipHandler, SubnetworkAssignations};

use crate::{
    SessionStatus,
    addressbook::{AddressBookMut, AddressBookSnapshot},
    membership::{Assignations, handler::DaMembershipHandler},
};

#[async_trait::async_trait]
pub trait MembershipStorageAdapter<Id, NetworkId> {
    type StorageService: ServiceData;

    fn new(relay: OutboundRelay<<Self::StorageService as ServiceData>::Message>) -> Self;

    async fn store(
        &self,
        session_id: SessionNumber,
        assignations: Assignations<Id, NetworkId>,
        provider_mappings: HashMap<Id, ProviderId>,
    ) -> Result<(), DynError>;

    async fn get(
        &self,
        session_id: SessionNumber,
    ) -> Result<Option<Assignations<Id, NetworkId>>, DynError>;

    async fn store_addresses(&self, ids: HashMap<Id, Multiaddr>) -> Result<(), DynError>;

    async fn get_address(&self, id: Id) -> Result<Option<Multiaddr>, DynError>;

    async fn get_provider_id(&self, id: Id) -> Result<Option<ProviderId>, DynError>;

    async fn prune(&self, cutoff_session: SessionNumber) -> Result<(), DynError>;
}

pub struct MembershipStorage<MembershipAdapter, Membership, AddressBook> {
    membership_adapter: MembershipAdapter,
    membership_handler: DaMembershipHandler<Membership>,
    addressbook: AddressBook,
    min_session_members: usize,
}

impl<MembershipAdapter, Membership, AddressBook>
    MembershipStorage<MembershipAdapter, Membership, AddressBook>
where
    MembershipAdapter: MembershipStorageAdapter<
            <Membership as MembershipHandler>::Id,
            <Membership as MembershipHandler>::NetworkId,
        > + Send
        + Sync,
    Membership: MembershipCreator + Clone + Send + Sync,
    Membership::Id: Send + Sync + Clone + Copy + Eq + Hash,
    AddressBook: AddressBookMut<Id = Membership::Id> + Send + Sync,
{
    pub const fn new(
        membership_adapter: MembershipAdapter,
        membership_handler: DaMembershipHandler<Membership>,
        addressbook: AddressBook,
        min_session_members: usize,
    ) -> Self {
        Self {
            membership_adapter,
            membership_handler,
            addressbook,
            min_session_members,
        }
    }

    pub async fn update(
        &self,
        session_id: SessionNumber,
        new_members: AddressBookSnapshot<Membership::Id>,
        provider_mappings: HashMap<Membership::Id, ProviderId>,
    ) -> Result<SessionStatus, DynError> {
        let mut hasher = Blake2b512::default();
        BlakeUpdate::update(&mut hasher, session_id.to_le_bytes().as_slice());
        let seed: [u8; 64] = hasher.finalize().into();

        let update: HashSet<Membership::Id> = new_members.keys().copied().collect();

        let (membership_state, updated_membership, assignations) = {
            if provider_mappings.len() < self.min_session_members {
                let updated_membership = self
                    .membership_handler
                    .membership()
                    .init(session_id, SubnetworkAssignations::new());
                (
                    SessionStatus::InsufficientMembers,
                    updated_membership,
                    SubnetworkAssignations::new(),
                )
            } else {
                let mut rng = BlakeRng::from_seed(seed.into());
                let updated_membership = self
                    .membership_handler
                    .membership()
                    .update(session_id, update, &mut rng);
                let assignations = updated_membership.subnetworks();
                (
                    SessionStatus::SufficientMembers,
                    updated_membership,
                    assignations,
                )
            }
        };

        tracing::debug!("Updating membership at session {session_id} with {assignations:?}");

        // update in-memory latest membership
        self.membership_handler.update(updated_membership.clone());
        self.addressbook.update(new_members.clone());

        // update membership storage
        self.membership_adapter
            .store(session_id, assignations, provider_mappings)
            .await?;
        self.membership_adapter.store_addresses(new_members).await?;

        Ok(membership_state)
    }

    pub async fn get_historic_membership(
        &self,
        session_id: SessionNumber,
    ) -> Result<Option<Membership>, DynError> {
        let mut membership = None;

        if let Some(assignations) = self.membership_adapter.get(session_id).await? {
            membership = Some(
                self.membership_handler
                    .membership()
                    .init(session_id, assignations),
            );
        }

        if membership.is_none() {
            tracing::debug!("No membership found for session {session_id}");
            return Ok(None);
        }

        Ok(Some(membership.unwrap()))
    }

    pub async fn prune(&self, cutoff_session: SessionNumber) -> Result<(), DynError> {
        self.membership_adapter.prune(cutoff_session).await
    }
}

#[async_trait::async_trait]
impl<Id, NetworkId, T> MembershipStorageAdapter<Id, NetworkId> for Arc<T>
where
    T: MembershipStorageAdapter<Id, NetworkId> + Send + Sync,
    Id: Send + Sync + 'static,
    NetworkId: Send + Sync + 'static,
{
    type StorageService = T::StorageService;

    fn new(relay: OutboundRelay<<Self::StorageService as ServiceData>::Message>) -> Self {
        Self::new(T::new(relay))
    }

    async fn store(
        &self,
        session_id: SessionNumber,
        assignations: Assignations<Id, NetworkId>,
        provider_mappings: HashMap<Id, ProviderId>,
    ) -> Result<(), DynError> {
        (**self)
            .store(session_id, assignations, provider_mappings)
            .await
    }

    async fn get(
        &self,
        session_id: SessionNumber,
    ) -> Result<Option<Assignations<Id, NetworkId>>, DynError> {
        (**self).get(session_id).await
    }

    async fn store_addresses(&self, ids: HashMap<Id, Multiaddr>) -> Result<(), DynError> {
        (**self).store_addresses(ids).await
    }

    async fn get_address(&self, id: Id) -> Result<Option<Multiaddr>, DynError> {
        (**self).get_address(id).await
    }

    async fn get_provider_id(&self, id: Id) -> Result<Option<ProviderId>, DynError> {
        (**self).get_provider_id(id).await
    }

    async fn prune(&self, cutoff_session: SessionNumber) -> Result<(), DynError> {
        (**self).prune(cutoff_session).await
    }
}
