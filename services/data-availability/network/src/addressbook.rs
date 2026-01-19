use std::{collections::HashMap, sync::Arc};

use arc_swap::ArcSwap;
use lb_da_network_core::addressbook::AddressBookHandler;
use libp2p::{Multiaddr, PeerId};

pub type AddressBookSnapshot<Id> = HashMap<Id, Multiaddr>;

#[derive(Debug, Clone, Default)]
pub struct AddressBook {
    peers: Arc<ArcSwap<HashMap<PeerId, Multiaddr>>>,
}

impl AddressBookHandler for AddressBook {
    type Id = PeerId;

    fn get_address(&self, peer_id: &Self::Id) -> Option<Multiaddr> {
        self.peers.load().get(peer_id).cloned()
    }
}

pub trait AddressBookMut: AddressBookHandler {
    fn update(&self, new_peers: AddressBookSnapshot<Self::Id>);
}

impl AddressBookMut for AddressBook {
    fn update(&self, new_peers: AddressBookSnapshot<Self::Id>) {
        let mut new_map = (**self.peers.load()).clone();
        new_map.extend(new_peers);
        self.peers.store(Arc::new(new_map));
    }
}
