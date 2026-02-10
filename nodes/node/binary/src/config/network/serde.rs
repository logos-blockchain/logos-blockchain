use core::time::Duration;

use lb_libp2p::{Multiaddr, ed25519, gossipsub};
use serde::{Deserialize, Serialize};

// Definition copied from the `logos-blockchain-network` service settings,
// assuming the libp2p backend and removing the concrete protocol names, which
// will be injected via the deployment configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: BackendSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendSettings {
    pub swarm: SwarmConfig,
    // Initial peers to connect to
    #[serde(default)]
    pub initial_peers: Vec<Multiaddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Listening IPv4 address
    pub host: std::net::Ipv4Addr,
    /// UDP/QUIC listening port. Use 0 for random.
    pub port: u16,
    /// Ed25519 private key in hex format. Default: random.
    #[serde(
        with = "lb_libp2p::secret_key_serde",
        default = "ed25519::SecretKey::generate"
    )]
    pub node_key: ed25519::SecretKey,

    /// Gossipsub config
    #[serde(
        with = "lb_libp2p::config::gossipsub::ConfigDef",
        default = "lb_libp2p::gossipsub::Config::default"
    )]
    pub gossipsub_config: gossipsub::Config,

    /// Kademlia config (required; Identify must be enabled too)
    #[serde(default)]
    pub kademlia_config: KademliaSettings,

    /// Identify config (required)
    #[serde(default)]
    pub identify_config: IdentifySettings,

    /// Chain sync config
    #[serde(default)]
    pub chain_sync_config: cryptarchia_sync::Config,

    /// Nat config
    #[serde(default)]
    pub nat_config: NatSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KademliaSettings {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdentifySettings {
    /// Agent version string to advertise
    /// Default from libp2p: 'rust-libp2p/{version}'
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,

    /// Interval in seconds between pushes of identify info
    /// Default from libp2p: 5 minutes (300 seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,

    /// Whether new/expired listen addresses should trigger
    /// an active push of an identify message to all connected peers
    /// Default from libp2p: false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_listen_addr_updates: Option<bool>,

    /// How many entries of discovered peers to keep
    /// Default from libp2p: 100
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_size: Option<usize>,

    /// Whether to hide listen addresses in responses (only share external
    /// addresses) Default from libp2p: false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_listen_addrs: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NatSettings {
    /// NAT traversal with autonat, mapping, and gateway monitoring
    Traversal(TraversalSettings),
    /// Static external address for nodes with fixed public IPs
    Static {
        /// The fixed external address to use (NAT traversal disabled)
        external_address: libp2p::Multiaddr,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraversalSettings {
    pub autonat: AutonatClientSettings,
    pub mapping: MappingSettings,
    pub gateway_monitor: gateway::Settings,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct AutonatClientSettings {
    /// How many candidates we will test at most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_candidates: Option<usize>,

    /// The interval at which we will attempt to confirm candidates as external
    /// addresses, only used for new candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_interval_millisecs: Option<u64>,

    /// The interval at which we will retest successful external addresses.
    /// This is used to ensure that the external address is still valid and
    /// reachable.
    #[serde(default = "default_retest_interval")]
    pub retest_successful_external_addresses_interval: Duration,
}

const fn default_retest_interval() -> Duration {
    Duration::from_secs(60)
}

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MappingSettings {
    #[serde(default = "default_timeout")]
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub timeout: Duration,
    #[serde(default = "default_lifetime")]
    pub lease_duration: Duration,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_renewal_delay_fraction")]
    pub renewal_delay_fraction: PositiveF64,
    #[serde(default = "default_retry_interval")]
    pub retry_interval: Duration,
}

const fn default_timeout() -> Duration {
    Duration::from_secs(1)
}

const fn default_lifetime() -> Duration {
    Duration::from_secs(7200) // 2 hours
}

const fn default_max_retries() -> u32 {
    3
}

fn default_renewal_delay_fraction() -> PositiveF64 {
    PositiveF64::try_from(0.8).expect("0.8 is positive")
}

const fn default_retry_interval() -> Duration {
    Duration::from_secs(30)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            lease_duration: default_lifetime(),
            max_retries: default_max_retries(),
            renewal_delay_fraction: default_renewal_delay_fraction(),
            retry_interval: default_retry_interval(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GatewaySettings {
    /// How often to check for gateway address changes
    #[serde(default = "default_check_interval")]
    pub check_interval: Duration,
}

const fn default_check_interval() -> Duration {
    Duration::from_secs(300)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_interval: default_check_interval(),
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChainSyncConfig {
    /// The maximum duration to wait for a peer to respond
    /// with a message.
    #[serde_as(as = "DurationMilliSeconds<u64>")]
    #[serde(default = "default_response_timeout")]
    pub peer_response_timeout: Duration,
}

const fn default_response_timeout() -> Duration {
    Duration::from_secs(5)
}
