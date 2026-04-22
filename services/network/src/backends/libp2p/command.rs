use std::collections::HashSet;

use lb_libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub use crate::backends::libp2p::swarm::{ChainSyncCommand, DiscoveryCommand, PubSubCommand};

#[derive(Debug)]
#[non_exhaustive]
pub enum NetworkCommand {
    Connect(Dial),
    Info {
        reply: oneshot::Sender<Libp2pInfo>,
    },
    ConnectedPeers {
        reply: oneshot::Sender<HashSet<PeerId>>,
    },
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    PubSub(PubSubCommand),
    Discovery(DiscoveryCommand),
    Network(NetworkCommand),
    ChainSync(ChainSyncCommand),
}

#[derive(Debug)]
pub struct Dial {
    pub addr: Multiaddr,
    pub retry_count: usize,
    pub result_sender: oneshot::Sender<Result<PeerId, lb_libp2p::DialError>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Libp2pInfo {
    pub listen_addresses: Vec<Multiaddr>,
    pub peer_id: PeerId,
    pub connected_peers: Vec<PeerId>,
    pub n_peers: usize,
    pub n_connections: u32,
    pub n_pending_connections: u32,
}
