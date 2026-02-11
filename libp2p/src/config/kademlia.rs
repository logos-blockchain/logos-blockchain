use std::{num::NonZeroUsize, time::Duration};

use libp2p::{StreamProtocol, kad};
use serde::{Deserialize, Serialize};

/// A serializable representation of Kademlia configuration options.
/// When a value is None, the libp2p defaults are used.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "config")]
pub enum CachingSettings {
    Disabled,
    Enabled { max_peers: u16 },
}

impl Settings {
    #[must_use]
    pub fn to_libp2p_config(&self, protocol_name: StreamProtocol) -> kad::Config {
        let mut config = kad::Config::new(protocol_name);

        if let Some(timeout) = self.query_timeout_secs {
            config.set_query_timeout(Duration::from_secs(timeout));
        }

        if let Some(replication_factor) = self.replication_factor {
            config.set_replication_factor(replication_factor);
        }

        if let Some(parallelism) = self.parallelism {
            config.set_parallelism(parallelism);
        }

        if let Some(disjoint) = self.disjoint_query_paths {
            config.disjoint_query_paths(disjoint);
        }

        if let Some(size) = self.max_packet_size {
            config.set_max_packet_size(size);
        }

        if let Some(inserts) = &self.kbucket_inserts {
            match inserts {
                KBucketInserts::OnConnected => {
                    config.set_kbucket_inserts(kad::BucketInserts::OnConnected);
                }
                KBucketInserts::Manual => {
                    config.set_kbucket_inserts(kad::BucketInserts::Manual);
                }
            }
        }

        if let Some(caching) = &self.caching {
            match caching {
                CachingSettings::Disabled => {
                    config.set_caching(kad::Caching::Disabled);
                }
                CachingSettings::Enabled { max_peers } => {
                    config.set_caching(kad::Caching::Enabled {
                        max_peers: *max_peers,
                    });
                }
            }
        }

        // Set the periodic bootstrap interval if provided
        // If not provided, use the libp2p default
        // disable periodic bootstrap if interval is 0
        if let Some(interval) = &self.periodic_bootstrap_interval_secs {
            if *interval == 0 {
                config.set_periodic_bootstrap_interval(None);
            } else {
                config.set_periodic_bootstrap_interval(Some(Duration::from_secs(*interval)));
            }
        }

        config
    }
}
