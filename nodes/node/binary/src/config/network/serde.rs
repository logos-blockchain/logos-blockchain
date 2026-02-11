use core::{num::NonZeroUsize, time::Duration};

use lb_libp2p::{Multiaddr, ed25519, gossipsub, protocol_name::StreamProtocol};
use lb_utils::{
    bounded_duration::{MinimalBoundedDuration, SECOND},
    math::PositiveF64,
};
use serde::{Deserialize, Serialize};
use serde_with::{DurationMilliSeconds, serde_as};

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
    pub gossipsub_config: GossipsubSettings,

    pub kad_protocol_name: StreamProtocol,
    pub identify_protocol_name: StreamProtocol,
    pub chain_sync_protocol_name: StreamProtocol,

    /// Kademlia config (required; Identify must be enabled too)
    #[serde(default)]
    pub kademlia_config: KademliaSettings,

    /// Identify config (required)
    #[serde(default)]
    pub identify_config: IdentifySettings,

    /// Chain sync config
    #[serde(default)]
    pub chain_sync_config: ChainSyncConfig,

    /// Nat config
    #[serde(default)]
    pub nat_config: NatSettings,
}

impl From<SwarmConfig> for lb_libp2p::SwarmConfig {
    fn from(value: SwarmConfig) -> Self {
        Self {
            host: value.host,
            port: value.port,
            node_key: value.node_key,
            gossipsub_config: value.gossipsub_config.into(),
            chain_sync_protocol_name: value.chain_sync_protocol_name,
            identify_protocol_name: value.identify_protocol_name,
            kad_protocol_name: value.kad_protocol_name,
            kademlia_config: value.kademlia_config.into(),
            identify_config: value.identify_config.into(),
            chain_sync_config: value.chain_sync_config.into(),
            nat_config: value.nat_config.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Type matching `libp2p::gossipsub::Config` for remote serde impl."
)]
pub struct GossipsubSettings {
    history_length: usize,
    history_gossip: usize,
    mesh_n: usize,
    mesh_n_low: usize,
    mesh_n_high: usize,
    retain_scores: usize,
    gossip_lazy: usize,
    gossip_factor: f64,
    heartbeat_initial_delay: Duration,
    heartbeat_interval: Duration,
    fanout_ttl: Duration,
    check_explicit_peers_ticks: u64,
    duplicate_cache_time: Duration,
    validate_messages: bool,
    allow_self_origin: bool,
    do_px: bool,
    prune_peers: usize,
    prune_backoff: Duration,
    unsubscribe_backoff: Duration,
    backoff_slack: u32,
    flood_publish: bool,
    graft_flood_threshold: Duration,
    mesh_outbound_min: usize,
    opportunistic_graft_ticks: u64,
    opportunistic_graft_peers: usize,
    gossip_retransimission: u32,
    max_messages_per_rpc: Option<usize>,
    max_ihave_length: usize,
    max_ihave_messages: usize,
    iwant_followup_time: Duration,
    published_message_ids_cache_time: Duration,
}

#[expect(
    clippy::fallible_impl_from,
    reason = "`TryFrom` impl conflicting with blanket impl."
)]
impl From<GossipsubSettings> for gossipsub::Config {
    fn from(def: GossipsubSettings) -> Self {
        let mut builder = gossipsub::ConfigBuilder::default();
        let mut builder = builder
            .allow_self_origin(true)
            .history_length(def.history_length)
            .history_gossip(def.history_gossip)
            .mesh_n(def.mesh_n)
            .mesh_n_low(def.mesh_n_low)
            .mesh_n_high(def.mesh_n_high)
            .retain_scores(def.retain_scores)
            .gossip_lazy(def.gossip_lazy)
            .gossip_factor(def.gossip_factor)
            .heartbeat_initial_delay(def.heartbeat_initial_delay)
            .heartbeat_interval(def.heartbeat_interval)
            .fanout_ttl(def.fanout_ttl)
            .check_explicit_peers_ticks(def.check_explicit_peers_ticks)
            .duplicate_cache_time(def.duplicate_cache_time)
            .allow_self_origin(def.allow_self_origin)
            .prune_peers(def.prune_peers)
            .prune_backoff(def.prune_backoff)
            .unsubscribe_backoff(def.unsubscribe_backoff.as_secs())
            .backoff_slack(def.backoff_slack)
            .flood_publish(def.flood_publish)
            .graft_flood_threshold(def.graft_flood_threshold)
            .mesh_outbound_min(def.mesh_outbound_min)
            .opportunistic_graft_ticks(def.opportunistic_graft_ticks)
            .opportunistic_graft_peers(def.opportunistic_graft_peers)
            .gossip_retransimission(def.gossip_retransimission)
            .max_messages_per_rpc(def.max_messages_per_rpc)
            .max_ihave_length(def.max_ihave_length)
            .max_ihave_messages(def.max_ihave_messages)
            .iwant_followup_time(def.iwant_followup_time)
            .published_message_ids_cache_time(def.published_message_ids_cache_time);

        if def.validate_messages {
            builder = builder.validate_messages();
        }
        if def.do_px {
            builder = builder.do_px();
        }

        builder.build().unwrap()
    }
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

impl From<KademliaSettings> for lb_libp2p::KademliaSettings {
    fn from(value: KademliaSettings) -> Self {
        Self {
            query_timeout_secs: value.query_timeout_secs,
            replication_factor: value.replication_factor,
            parallelism: value.parallelism,
            disjoint_query_paths: value.disjoint_query_paths,
            max_packet_size: value.max_packet_size,
            kbucket_inserts: value.kbucket_inserts.map(Into::into),
            caching: value.caching.map(Into::into),
            periodic_bootstrap_interval_secs: value.periodic_bootstrap_interval_secs,
            client_mode: value.client_mode,
        }
    }
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

impl From<IdentifySettings> for lb_libp2p::IdentifySettings {
    fn from(value: IdentifySettings) -> Self {
        Self {
            agent_version: value.agent_version,
            interval_secs: value.interval_secs,
            push_listen_addr_updates: value.push_listen_addr_updates,
            cache_size: value.cache_size,
            hide_listen_addrs: value.hide_listen_addrs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NatSettings {
    /// NAT traversal with autonat, mapping, and gateway monitoring
    Traversal(TraversalSettings),
    /// Static external address for nodes with fixed public IPs
    Static {
        /// The fixed external address to use (NAT traversal disabled)
        external_address: Multiaddr,
    },
}

impl Default for NatSettings {
    fn default() -> Self {
        Self::Traversal(TraversalSettings::default())
    }
}

impl From<NatSettings> for lb_libp2p::NatSettings {
    fn from(value: NatSettings) -> Self {
        match value {
            NatSettings::Traversal(traversal) => Self::Traversal(traversal.into()),
            NatSettings::Static { external_address } => Self::Static { external_address },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraversalSettings {
    pub autonat: AutonatClientSettings,
    pub mapping: MappingSettings,
    pub gateway_monitor: GatewaySettings,
}

impl From<TraversalSettings> for lb_libp2p::TraversalSettings {
    fn from(value: TraversalSettings) -> Self {
        Self {
            autonat: value.autonat.into(),
            mapping: value.mapping.into(),
            gateway_monitor: value.gateway_monitor.into(),
        }
    }
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

impl From<AutonatClientSettings> for lb_libp2p::AutonatClientSettings {
    fn from(value: AutonatClientSettings) -> Self {
        Self {
            max_candidates: value.max_candidates,
            probe_interval_millisecs: value.probe_interval_millisecs,
            retest_successful_external_addresses_interval: value
                .retest_successful_external_addresses_interval,
        }
    }
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

impl Default for MappingSettings {
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

impl From<MappingSettings> for lb_libp2p::NatMappingSettings {
    fn from(value: MappingSettings) -> Self {
        Self {
            timeout: value.timeout,
            lease_duration: value.lease_duration,
            max_retries: value.max_retries,
            renewal_delay_fraction: value.renewal_delay_fraction,
            retry_interval: value.retry_interval,
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

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            check_interval: default_check_interval(),
        }
    }
}

impl From<GatewaySettings> for lb_libp2p::GatewaySettings {
    fn from(value: GatewaySettings) -> Self {
        Self {
            check_interval: value.check_interval,
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

impl From<ChainSyncConfig> for lb_libp2p::cryptarchia_sync::Config {
    fn from(value: ChainSyncConfig) -> Self {
        Self {
            peer_response_timeout: value.peer_response_timeout,
        }
    }
}
