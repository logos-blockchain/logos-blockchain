pub mod chain;
pub mod node_id;
pub mod service;

use core::fmt::{self, Debug, Formatter};

use lb_blend::scheduling::membership::Membership;
use lb_core::crypto::ZkHash;
use lb_groth16::fr_to_bytes;
use lb_poq::CorePathAndSelectors;

#[derive(Clone, Debug)]
pub struct MembershipInfo<NodeId> {
    pub membership: Membership<NodeId>,
    // `None` if membership is empty.
    pub zk: Option<ZkInfo>,
}

#[cfg(test)]
impl<NodeId> From<Membership<NodeId>> for MembershipInfo<NodeId> {
    fn from(membership: Membership<NodeId>) -> Self {
        let zk = if membership.is_empty() {
            None
        } else {
            Some(ZkInfo::default())
        };

        Self { membership, zk }
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Default))]
/// ZK info for a new epoch.
pub struct ZkInfo {
    /// The merkle root of the ZK public keys of all core nodes.
    pub root: ZkHash,
    /// The merkle path (and selectors) proving the node's ZK public key is part
    /// of the epoch merkle tree. This is `None` for edge nodes.
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
