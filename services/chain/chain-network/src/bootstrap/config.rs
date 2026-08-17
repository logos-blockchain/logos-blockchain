use std::{collections::HashSet, hash::Hash, time::Duration};

use serde::{Deserialize, Serialize};

#[serde_with::serde_as]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BootstrapConfig<NodeId>
where
    NodeId: Clone + Eq + Hash,
{
    pub ibd: IbdConfig<NodeId>,
}

/// IBD configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IbdConfig<NodeId>
where
    NodeId: Clone + Eq + Hash,
{
    /// Peers to query for the chain tip during IBD.
    pub peers: HashSet<NodeId>,
    /// Whether IBD should discover currently-connected peers when the
    /// configured peer set is empty.
    ///
    /// This is used for bootstrap addresses that do not carry a libp2p
    /// `PeerId`: the network can still connect to them and expose their tips
    /// through `NetworkAdapter::sample_tips`.
    pub discover_connected_peers: bool,
    /// Maximum number of connected peers to sample for discovery-based IBD.
    pub max_connected_peers_to_sample: usize,
    /// Maximum number of attempts when fetching tips from IBD peers.
    pub tips_fetch_max_attempts: usize,
    /// Lower bound of the exponential backoff between tip-fetch attempts.
    pub tips_fetch_min_delay: Duration,
    /// Upper bound of the exponential backoff between tip-fetch attempts.
    pub tips_fetch_max_delay: Duration,
    /// Delay between IBD rounds.
    pub round_delay: Duration,
}
