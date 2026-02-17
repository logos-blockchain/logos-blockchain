use core::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

use crate::config::utils;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    /// The timeout for a single query in seconds
    #[serde(skip_serializing_if = "utils::is_default")]
    pub query_timeout_secs: Option<u64>,
    /// The replication factor to use
    #[serde(skip_serializing_if = "utils::is_default")]
    pub replication_factor: Option<NonZeroUsize>,
    /// The allowed level of parallelism for iterative queries
    #[serde(skip_serializing_if = "utils::is_default")]
    pub parallelism: Option<NonZeroUsize>,
    /// Require iterative queries to use disjoint paths
    #[serde(skip_serializing_if = "utils::is_default")]
    pub disjoint_query_paths: Option<bool>,
    /// Maximum allowed size of individual Kademlia packets
    #[serde(skip_serializing_if = "utils::is_default")]
    pub max_packet_size: Option<usize>,
    /// The k-bucket insertion strategy
    #[serde(skip_serializing_if = "utils::is_default")]
    pub kbucket_inserts: Option<KBucketInserts>,
    /// The caching strategy
    #[serde(skip_serializing_if = "utils::is_default")]
    pub caching: Option<CachingSettings>,
    /// The interval in seconds for periodic bootstrap
    /// If enabled the periodic bootstrap will run every x seconds in addition
    /// to the automatic bootstrap that is triggered when a new peer is added
    #[serde(skip_serializing_if = "utils::is_default")]
    pub periodic_bootstrap_interval_secs: Option<u64>,
    /// The Kademlia node is in client mode if it does not
    /// expose its own Kademlia ID and only connects to other nodes
    #[serde(skip_serializing_if = "utils::is_default")]
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
