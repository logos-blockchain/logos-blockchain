pub mod addressbook;
#[expect(
    clippy::too_many_arguments,
    reason = "Behaviours needs configuration passed for multiple protocols"
)]
pub mod behaviour;
pub mod maintenance;
pub mod protocol;
pub mod protocols;
#[expect(
    clippy::too_many_arguments,
    reason = "Swarm needs configuration passed for multiple behaviours"
)]
pub mod swarm;
#[cfg(test)]
pub mod test_utils;

pub use libp2p::PeerId;
pub use subnetworks_assignations::SubnetworkId;
