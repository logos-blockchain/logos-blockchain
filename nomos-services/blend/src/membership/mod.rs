pub mod node_id;
pub mod service;

use std::pin::Pin;

use futures::Stream;
use key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};
use nomos_blend::scheduling::membership::Membership;
use nomos_core::crypto::ZkHash;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use poq::CorePathAndSelectors;

#[derive(Clone, Debug)]
pub struct MembershipInfo<NodeId> {
    pub membership: Membership<NodeId>,
    pub zk: ZkInfo,
    pub session_number: u64,
}

impl<NodeId> MembershipInfo<NodeId> {
    #[cfg(test)]
    #[must_use]
    pub fn from_membership_and_session_number(
        membership: Membership<NodeId>,
        session_number: u64,
    ) -> Self {
        Self {
            membership,
            session_number,
            zk: ZkInfo::default(),
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(test, derive(Default))]
/// ZK info for a new session.
pub struct ZkInfo {
    /// The merkle root of the ZK public keys of all core nodes.
    pub root: ZkHash,
    /// The merkle path (and selectors) proving the node's ZK public key is part
    /// of the session merkle tree. This is `None` for edge nodes.
    pub core_and_path_selectors: Option<CorePathAndSelectors>,
}

pub type MembershipStream<NodeId> =
    Pin<Box<dyn Stream<Item = MembershipInfo<NodeId>> + Send + Sync + 'static>>;

pub type ServiceMessage<MembershipAdapter> =
    <<MembershipAdapter as Adapter>::Service as ServiceData>::Message;

/// An adapter for the membership service.
#[async_trait::async_trait]
pub trait Adapter {
    type Service: ServiceData;
    type NodeId;
    type Error: std::error::Error;

    fn new(
        relay: OutboundRelay<ServiceMessage<Self>>,
        signing_public_key: Ed25519PublicKey,
        zk_public_key: Option<ZkPublicKey>,
    ) -> Self;

    /// Subscribe to membership updates.
    async fn subscribe(&self) -> Result<MembershipStream<Self::NodeId>, Self::Error>;
}
