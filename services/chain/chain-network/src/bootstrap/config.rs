use std::{collections::HashSet, hash::Hash, time::Duration};

#[serde_with::serde_as]
#[derive(Debug, Clone)]
pub struct BootstrapConfig<NodeId>
where
    NodeId: Clone + Eq + Hash,
{
    pub ibd: IbdConfig<NodeId>,
}

/// IBD configuration.
#[derive(Debug, Clone)]
pub struct IbdConfig<NodeId>
where
    NodeId: Clone + Eq + Hash,
{
    /// Peers to download blocks from.
    pub peers: HashSet<NodeId>,
    /// Delay before attempting the next download
    /// when no download is needed at the moment from a peer.
    pub delay_before_new_download: Duration,
}
