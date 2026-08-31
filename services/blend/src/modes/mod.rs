mod broadcast;

use core::hash::Hash;
use std::fmt::Debug;

use lb_blend::scheduling::membership::Membership;
use lb_log_targets::blend;

pub use crate::modes::broadcast::{BroadcastMode, run_broadcast_mode};

const LOG_TARGET: &str = blend::service::MODES;

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Core,
    Edge,
    Broadcast,
}

impl Mode {
    pub fn choose<NodeId>(membership: &Membership<NodeId>, minimal_network_size: usize) -> Self
    where
        NodeId: Eq + Hash,
    {
        if membership.size() < minimal_network_size {
            Self::Broadcast
        } else if membership.contains_local() {
            Self::Core
        } else {
            Self::Edge
        }
    }
}
