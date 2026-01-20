use std::{fmt::Debug, sync::Arc};

use libp2p::Multiaddr;

pub trait AddressBookHandler {
    type Id: Debug;

    fn get_address(&self, peer_id: &Self::Id) -> Option<Multiaddr>;
}

impl<T> AddressBookHandler for Arc<T>
where
    T: AddressBookHandler,
{
    type Id = T::Id;

    fn get_address(&self, peer_id: &Self::Id) -> Option<Multiaddr> {
        (**self).get_address(peer_id)
    }
}
