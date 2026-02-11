use core::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// The timeout for a single query in seconds
    /// Default from libp2p: 60 seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_timeout_secs: Option<u64>,

    /// The replication factor to use
    /// Default from libp2p: 20 (`K_VALUE`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replication_factor: Option<NonZeroUsize>,

    /// The allowed level of parallelism for iterative queries
    /// Default from libp2p: 3 (`ALPHA_VALUE`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<NonZeroUsize>,

    /// Require iterative queries to use disjoint paths
    /// Default from libp2p: false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disjoint_query_paths: Option<bool>,

    /// Maximum allowed size of individual Kademlia packets
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_packet_size: Option<usize>,

    /// The k-bucket insertion strategy
    /// Default from libp2p: "`on_connected`"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kbucket_inserts: Option<KBucketInserts>,

    /// The caching strategy
    /// Default from libp2p: Enabled with `max_peers=1`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caching: Option<CachingSettings>,

    /// The interval in seconds for periodic bootstrap
    /// If enabled the periodic bootstrap will run every x seconds in addition
    /// to the automatic bootstrap that is triggered when a new peer is added
    /// Default from libp2p: 5 minutes (300 seconds)
    /// None means use libp2p default
    /// Some(0) means periodic bootstrap is disabled
    /// Some(x) means periodic bootstrap is enabled for x seconds period
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub periodic_bootstrap_interval_secs: Option<u64>,

    /// The Kademlia node is in client mode if it does not
    /// expose its own Kademlia ID and only connects to other nodes
    /// Default from libp2p: false (server mode)
    #[serde(default)]
    pub client_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KBucketInserts {
    OnConnected,
    Manual,
}

impl From<KBucketInserts> for lb_libp2p::config::KBucketInserts {
    fn from(value: KBucketInserts) -> Self {
        match value {
            KBucketInserts::OnConnected => Self::OnConnected,
            KBucketInserts::Manual => Self::Manual,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "config")]
pub enum CachingSettings {
    Disabled,
    Enabled { max_peers: u16 },
}

impl From<CachingSettings> for lb_libp2p::config::CachingSettings {
    fn from(value: CachingSettings) -> Self {
        match value {
            CachingSettings::Disabled => Self::Disabled,
            CachingSettings::Enabled { max_peers } => Self::Enabled { max_peers },
        }
    }
}
