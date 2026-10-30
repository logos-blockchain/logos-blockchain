use core::time::Duration;

use lb_utils::bounded_duration::{MILLI, MinimalBoundedDuration, SECOND};
use libp2p::gossipsub;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

/// How long gossipsub remembers seen message IDs before accepting the same
/// message again.
///
/// Upstream libp2p defaults to 60 seconds, which is far shorter than the time
/// a mempool transaction can stay in flight. Once the entry expires, a peer
/// re-broadcasting the same transaction is treated as new and the message is
/// re-propagated through the mesh, so we keep entries for a full day.
const DEFAULT_DUPLICATE_CACHE_TIME: Duration = Duration::from_hours(24);

// A partial copy of gossipsub::Config for deriving Serialize/Deserialize
// remotely https://serde.rs/remote-derive.html
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Matches gossipsub::Config fields"
)]
pub struct Config {
    pub history_length: usize,
    pub history_gossip: usize,
    pub mesh_n: usize,
    pub mesh_n_low: usize,
    pub mesh_n_high: usize,
    pub retain_scores: usize,
    pub gossip_lazy: usize,
    pub gossip_factor: f64,
    #[serde_as(as = "MinimalBoundedDuration<0, MILLI>")]
    pub heartbeat_initial_delay: Duration,
    #[serde_as(as = "MinimalBoundedDuration<0, MILLI>")]
    pub heartbeat_interval: Duration,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub fanout_ttl: Duration,
    pub check_explicit_peers_ticks: u64,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub duplicate_cache_time: Duration,
    pub validate_messages: bool,
    pub allow_self_origin: bool,
    pub do_px: bool,
    pub prune_peers: usize,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub prune_backoff: Duration,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub unsubscribe_backoff: Duration,
    pub backoff_slack: u32,
    pub flood_publish: bool,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub graft_flood_threshold: Duration,
    pub mesh_outbound_min: usize,
    pub opportunistic_graft_ticks: u64,
    pub opportunistic_graft_peers: usize,
    pub gossip_retransimission: u32,
    pub max_messages_per_rpc: Option<usize>,
    pub max_ihave_length: usize,
    pub max_ihave_messages: usize,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub iwant_followup_time: Duration,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub published_message_ids_cache_time: Duration,
}

impl Default for Config {
    fn default() -> Self {
        let inner_default = gossipsub::Config::default();
        Self {
            allow_self_origin: inner_default.allow_self_origin(),
            history_length: inner_default.history_length(),
            history_gossip: inner_default.history_gossip(),
            mesh_n: inner_default.mesh_n(),
            mesh_n_low: inner_default.mesh_n_low(),
            mesh_n_high: inner_default.mesh_n_high(),
            retain_scores: inner_default.retain_scores(),
            gossip_lazy: inner_default.gossip_lazy(),
            gossip_factor: inner_default.gossip_factor(),
            heartbeat_initial_delay: inner_default.heartbeat_initial_delay(),
            heartbeat_interval: inner_default.heartbeat_interval(),
            fanout_ttl: inner_default.fanout_ttl(),
            check_explicit_peers_ticks: inner_default.check_explicit_peers_ticks(),
            duplicate_cache_time: DEFAULT_DUPLICATE_CACHE_TIME,
            validate_messages: inner_default.validate_messages(),
            do_px: inner_default.do_px(),
            prune_peers: inner_default.prune_peers(),
            prune_backoff: inner_default.prune_backoff(),
            unsubscribe_backoff: inner_default.unsubscribe_backoff(),
            backoff_slack: inner_default.backoff_slack(),
            flood_publish: inner_default.flood_publish(),
            graft_flood_threshold: inner_default.graft_flood_threshold(),
            mesh_outbound_min: inner_default.mesh_outbound_min(),
            opportunistic_graft_ticks: inner_default.opportunistic_graft_ticks(),
            opportunistic_graft_peers: inner_default.opportunistic_graft_peers(),
            gossip_retransimission: inner_default.gossip_retransimission(),
            max_messages_per_rpc: inner_default.max_messages_per_rpc(),
            max_ihave_length: inner_default.max_ihave_length(),
            max_ihave_messages: inner_default.max_ihave_messages(),
            iwant_followup_time: inner_default.iwant_followup_time(),
            published_message_ids_cache_time: inner_default.published_message_ids_cache_time(),
        }
    }
}

#[expect(
    clippy::fallible_impl_from,
    reason = "`TryFrom` impl conflicting with blanket impl."
)]
impl From<Config> for gossipsub::Config {
    fn from(value: Config) -> Self {
        let Config {
            allow_self_origin,
            backoff_slack,
            check_explicit_peers_ticks,
            duplicate_cache_time,
            fanout_ttl,
            gossip_factor,
            gossip_lazy,
            do_px,
            flood_publish,
            gossip_retransimission,
            graft_flood_threshold,
            heartbeat_initial_delay,
            heartbeat_interval,
            history_gossip,
            history_length,
            iwant_followup_time,
            max_ihave_length,
            max_ihave_messages,
            max_messages_per_rpc,
            mesh_n,
            mesh_n_high,
            mesh_n_low,
            mesh_outbound_min,
            opportunistic_graft_peers,
            opportunistic_graft_ticks,
            prune_backoff,
            prune_peers,
            published_message_ids_cache_time,
            retain_scores,
            unsubscribe_backoff,
            validate_messages,
        } = value;

        let mut builder = gossipsub::ConfigBuilder::default();

        let mut builder = builder
            .history_length(history_length)
            .history_gossip(history_gossip)
            .mesh_n(mesh_n)
            .mesh_n_low(mesh_n_low)
            .mesh_n_high(mesh_n_high)
            .retain_scores(retain_scores)
            .gossip_lazy(gossip_lazy)
            .gossip_factor(gossip_factor)
            .heartbeat_initial_delay(heartbeat_initial_delay)
            .heartbeat_interval(heartbeat_interval)
            .fanout_ttl(fanout_ttl)
            .check_explicit_peers_ticks(check_explicit_peers_ticks)
            .duplicate_cache_time(duplicate_cache_time)
            .allow_self_origin(allow_self_origin)
            .prune_peers(prune_peers)
            .prune_backoff(prune_backoff)
            .unsubscribe_backoff(unsubscribe_backoff.as_secs())
            .backoff_slack(backoff_slack)
            .flood_publish(flood_publish)
            .graft_flood_threshold(graft_flood_threshold)
            .mesh_outbound_min(mesh_outbound_min)
            .opportunistic_graft_ticks(opportunistic_graft_ticks)
            .opportunistic_graft_peers(opportunistic_graft_peers)
            .gossip_retransimission(gossip_retransimission)
            .max_messages_per_rpc(max_messages_per_rpc)
            .max_ihave_length(max_ihave_length)
            .max_ihave_messages(max_ihave_messages)
            .iwant_followup_time(iwant_followup_time)
            .published_message_ids_cache_time(published_message_ids_cache_time);

        if validate_messages {
            builder = builder.validate_messages();
        }
        if do_px {
            builder = builder.do_px();
        }

        builder.build().unwrap()
    }
}
