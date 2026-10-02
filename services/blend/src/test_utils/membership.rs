use std::hash::Hash;

use lb_blend::scheduling::membership::{Membership, Node};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_cryptarchia_engine::era::Era;
use lb_groth16::AdditiveGroup as _;
use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use libp2p::Multiaddr;

use crate::membership::{
    MembershipInfo,
    chain::{BlendEpoch, BlendEpochState},
};

/// Epoch 0 of the genesis era, with `membership_info`.
pub fn blend_epoch<NodeId>(membership_info: MembershipInfo<NodeId>) -> BlendEpoch<NodeId> {
    (
        BlendEpochState {
            era: Era::GENESIS,
            epoch: Epoch::new(0),
            nonce: ZkHash::ZERO,
            aged: ZkHash::ZERO,
            lottery_0: ZkHash::ZERO,
            lottery_1: ZkHash::ZERO,
            pow_difficulty: ZkHash::ZERO,
        },
        membership_info,
    )
}

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
