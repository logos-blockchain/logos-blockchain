pub mod node_id;
pub mod service;

use core::fmt::{self, Debug, Formatter};
use std::pin::Pin;

use futures::Stream;
use lb_blend::scheduling::membership::Membership;
use lb_core::crypto::ZkHash;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};
use lb_poq::CorePathAndSelectors;
use overwatch::services::{ServiceData, relay::OutboundRelay};

#[derive(Clone, Debug)]
pub struct MembershipInfo<NodeId> {
    pub membership: Membership<NodeId>,
    // `None` if membership is empty.
    pub zk: Option<ZkInfo>,
    pub session_number: u64,
}

impl<NodeId> MembershipInfo<NodeId> {
    #[cfg(test)]
    #[must_use]
    pub fn from_membership_and_session_number(
        membership: Membership<NodeId>,
        session_number: u64,
    ) -> Self {
        let zk = if membership.is_empty() {
            None
        } else {
            Some(ZkInfo::default())
        };
        Self {
            membership,
            zk,
            session_number,
        }
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Default))]
/// ZK info for a new session.
pub struct ZkInfo {
    /// The merkle root of the ZK public keys of all core nodes.
    pub root: ZkHash,
    /// The merkle path (and selectors) proving the node's ZK public key is part
    /// of the session merkle tree. This is `None` for edge nodes.
    pub core_and_path_selectors: Option<CorePathAndSelectors>,
}

impl Debug for ZkInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZkInfo")
            .field("root", &hex::encode(fr_to_bytes(&self.root)))
            .field("core_and_path_selectors", &"<redacted>")
            .finish()
    }
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
