use core::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// The timeout for a single query in seconds
    pub query_timeout_secs: Option<u64>,

    /// The replication factor to use
    pub replication_factor: Option<NonZeroUsize>,

    /// The allowed level of parallelism for iterative queries
    pub parallelism: Option<NonZeroUsize>,

    /// Require iterative queries to use disjoint paths
    pub disjoint_query_paths: Option<bool>,

    /// Maximum allowed size of individual Kademlia packets
    pub max_packet_size: Option<usize>,

    /// The k-bucket insertion strategy
    pub kbucket_inserts: Option<KBucketInserts>,

    /// The caching strategy
    pub caching: Option<CachingSettings>,

    /// The interval in seconds for periodic bootstrap
    /// If enabled the periodic bootstrap will run every x seconds in addition
    /// to the automatic bootstrap that is triggered when a new peer is added
    pub periodic_bootstrap_interval_secs: Option<u64>,
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
