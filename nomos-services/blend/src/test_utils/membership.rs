use std::hash::Hash;

use key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use libp2p::Multiaddr;
use nomos_blend::scheduling::membership::{Membership, Node};

pub fn membership<NodeId>(ids: &[NodeId], local_id: NodeId) -> Membership<NodeId>
where
    NodeId: Clone + Eq + Hash,
    [u8; 32]: From<NodeId>,
{
    Membership::new(
        &ids.iter()
            .map(|id| Node {
                id: id.clone(),
                address: Multiaddr::empty(),
                public_key: key(id.clone()).1,
            })
            .collect::<Vec<_>>(),
        &key(local_id).1,
    )
}

pub fn key<NodeId>(id: NodeId) -> (UnsecuredEd25519Key, Ed25519PublicKey)
where
    [u8; 32]: From<NodeId>,
{
    let private_key = UnsecuredEd25519Key::from_bytes(&id.into());
    let public_key = private_key.public_key();
    (private_key, public_key)
}
