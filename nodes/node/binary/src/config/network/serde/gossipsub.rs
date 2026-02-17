#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::{cmp::Ordering, time::Duration};
use std::sync::LazyLock;

use libp2p::gossipsub;
use serde::{Deserialize, Serialize};

// A partial copy of gossipsub::Config for deriving Serialize/Deserialize
// remotely https://serde.rs/remote-derive.html
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Matches gossipsub::Config fields"
)]
pub struct Config {
    #[serde(skip_serializing_if = "is_default_history_length")]
    pub history_length: usize,
    #[serde(skip_serializing_if = "is_default_history_gossip")]
    pub history_gossip: usize,
    #[serde(skip_serializing_if = "is_default_mesh_n")]
    pub mesh_n: usize,
    #[serde(skip_serializing_if = "is_default_mesh_n_low")]
    pub mesh_n_low: usize,
    #[serde(skip_serializing_if = "is_default_mesh_n_high")]
    pub mesh_n_high: usize,
    #[serde(skip_serializing_if = "is_default_retain_scores")]
    pub retain_scores: usize,
    #[serde(skip_serializing_if = "is_default_gossip_lazy")]
    pub gossip_lazy: usize,
    #[serde(skip_serializing_if = "is_default_gossip_factor")]
    pub gossip_factor: f64,
    #[serde(skip_serializing_if = "is_default_heartbeat_initial_delay")]
    pub heartbeat_initial_delay: Duration,
    #[serde(skip_serializing_if = "is_default_heartbeat_interval")]
    pub heartbeat_interval: Duration,
    #[serde(skip_serializing_if = "is_default_fanout_ttl")]
    pub fanout_ttl: Duration,
    #[serde(skip_serializing_if = "is_default_check_explicit_peers_ticks")]
    pub check_explicit_peers_ticks: u64,
    #[serde(skip_serializing_if = "is_default_duplicate_cache_time")]
    pub duplicate_cache_time: Duration,
    #[serde(skip_serializing_if = "is_default_validate_messages")]
    pub validate_messages: bool,
    #[serde(skip_serializing_if = "is_default_allow_self_origin")]
    pub allow_self_origin: bool,
    #[serde(skip_serializing_if = "is_default_do_px")]
    pub do_px: bool,
    #[serde(skip_serializing_if = "is_default_prune_peers")]
    pub prune_peers: usize,
    #[serde(skip_serializing_if = "is_default_prune_backoff")]
    pub prune_backoff: Duration,
    #[serde(skip_serializing_if = "is_default_unsubscribe_backoff")]
    pub unsubscribe_backoff: Duration,
    #[serde(skip_serializing_if = "is_default_backoff_slack")]
    pub backoff_slack: u32,
    #[serde(skip_serializing_if = "is_default_flood_publish")]
    pub flood_publish: bool,
    #[serde(skip_serializing_if = "is_default_graft_flood_threshold")]
    pub graft_flood_threshold: Duration,
    #[serde(skip_serializing_if = "is_default_mesh_outbound_min")]
    pub mesh_outbound_min: usize,
    #[serde(skip_serializing_if = "is_default_opportunistic_graft_ticks")]
    pub opportunistic_graft_ticks: u64,
    #[serde(skip_serializing_if = "is_default_opportunistic_graft_peers")]
    pub opportunistic_graft_peers: usize,
    #[serde(skip_serializing_if = "is_default_gossip_retransimission")]
    pub gossip_retransimission: u32,
    #[serde(skip_serializing_if = "is_default_max_messages_per_rpc")]
    pub max_messages_per_rpc: Option<usize>,
    #[serde(skip_serializing_if = "is_default_max_ihave_length")]
    pub max_ihave_length: usize,
    #[serde(skip_serializing_if = "is_default_max_ihave_messages")]
    pub max_ihave_messages: usize,
    #[serde(skip_serializing_if = "is_default_iwant_followup_time")]
    pub iwant_followup_time: Duration,
    #[serde(skip_serializing_if = "is_default_published_message_ids_cache_time")]
    pub published_message_ids_cache_time: Duration,
}

static DEFAULT_CONFIG: LazyLock<gossipsub::Config> = LazyLock::new(gossipsub::Config::default);

fn is_default_history_length(history_length: &usize) -> bool {
    *history_length == DEFAULT_CONFIG.history_length()
}

fn is_default_history_gossip(history_gossip: &usize) -> bool {
    *history_gossip == DEFAULT_CONFIG.history_gossip()
}

fn is_default_mesh_n(mesh_n: &usize) -> bool {
    *mesh_n == DEFAULT_CONFIG.mesh_n()
}

fn is_default_mesh_n_low(mesh_n_low: &usize) -> bool {
    *mesh_n_low == DEFAULT_CONFIG.mesh_n_low()
}

fn is_default_mesh_n_high(mesh_n_high: &usize) -> bool {
    *mesh_n_high == DEFAULT_CONFIG.mesh_n_high()
}

fn is_default_retain_scores(retain_scores: &usize) -> bool {
    *retain_scores == DEFAULT_CONFIG.retain_scores()
}

fn is_default_gossip_lazy(gossip_lazy: &usize) -> bool {
    *gossip_lazy == DEFAULT_CONFIG.gossip_lazy()
}

fn is_default_gossip_factor(gossip_factor: &f64) -> bool {
    matches!(
        gossip_factor.partial_cmp(&DEFAULT_CONFIG.gossip_factor()),
        Some(Ordering::Equal)
    )
}

fn is_default_heartbeat_initial_delay(heartbeat_initial_delay: &Duration) -> bool {
    *heartbeat_initial_delay == DEFAULT_CONFIG.heartbeat_initial_delay()
}

fn is_default_heartbeat_interval(heartbeat_interval: &Duration) -> bool {
    *heartbeat_interval == DEFAULT_CONFIG.heartbeat_interval()
}

fn is_default_fanout_ttl(fanout_ttl: &Duration) -> bool {
    *fanout_ttl == DEFAULT_CONFIG.fanout_ttl()
}

fn is_default_check_explicit_peers_ticks(check_explicit_peers_ticks: &u64) -> bool {
    *check_explicit_peers_ticks == DEFAULT_CONFIG.check_explicit_peers_ticks()
}

fn is_default_duplicate_cache_time(duplicate_cache_time: &Duration) -> bool {
    *duplicate_cache_time == DEFAULT_CONFIG.duplicate_cache_time()
}

fn is_default_validate_messages(validate_messages: &bool) -> bool {
    *validate_messages == DEFAULT_CONFIG.validate_messages()
}

fn is_default_allow_self_origin(allow_self_origin: &bool) -> bool {
    *allow_self_origin == DEFAULT_CONFIG.allow_self_origin()
}

fn is_default_do_px(do_px: &bool) -> bool {
    *do_px == DEFAULT_CONFIG.do_px()
}

fn is_default_prune_peers(prune_peers: &usize) -> bool {
    *prune_peers == DEFAULT_CONFIG.prune_peers()
}

fn is_default_prune_backoff(prune_backoff: &Duration) -> bool {
    *prune_backoff == DEFAULT_CONFIG.prune_backoff()
}

fn is_default_unsubscribe_backoff(unsubscribe_backoff: &Duration) -> bool {
    *unsubscribe_backoff == DEFAULT_CONFIG.unsubscribe_backoff()
}

fn is_default_backoff_slack(backoff_slack: &u32) -> bool {
    *backoff_slack == DEFAULT_CONFIG.backoff_slack()
}

fn is_default_flood_publish(flood_publish: &bool) -> bool {
    *flood_publish == DEFAULT_CONFIG.flood_publish()
}

fn is_default_graft_flood_threshold(graft_flood_threshold: &Duration) -> bool {
    *graft_flood_threshold == DEFAULT_CONFIG.graft_flood_threshold()
}

fn is_default_mesh_outbound_min(mesh_outbound_min: &usize) -> bool {
    *mesh_outbound_min == DEFAULT_CONFIG.mesh_outbound_min()
}

fn is_default_opportunistic_graft_ticks(opportunistic_graft_ticks: &u64) -> bool {
    *opportunistic_graft_ticks == DEFAULT_CONFIG.opportunistic_graft_ticks()
}

fn is_default_opportunistic_graft_peers(opportunistic_graft_peers: &usize) -> bool {
    *opportunistic_graft_peers == DEFAULT_CONFIG.opportunistic_graft_peers()
}

fn is_default_gossip_retransimission(gossip_retransimission: &u32) -> bool {
    *gossip_retransimission == DEFAULT_CONFIG.gossip_retransimission()
}

#[expect(clippy::ref_option, reason = "Matches gossipsub::Config field type.")]
fn is_default_max_messages_per_rpc(max_messages_per_rpc: &Option<usize>) -> bool {
    *max_messages_per_rpc == DEFAULT_CONFIG.max_messages_per_rpc()
}

fn is_default_max_ihave_length(max_ihave_length: &usize) -> bool {
    *max_ihave_length == DEFAULT_CONFIG.max_ihave_length()
}

fn is_default_max_ihave_messages(max_ihave_messages: &usize) -> bool {
    *max_ihave_messages == DEFAULT_CONFIG.max_ihave_messages()
}

fn is_default_iwant_followup_time(iwant_followup_time: &Duration) -> bool {
    *iwant_followup_time == DEFAULT_CONFIG.iwant_followup_time()
}

fn is_default_published_message_ids_cache_time(
    published_message_ids_cache_time: &Duration,
) -> bool {
    *published_message_ids_cache_time == DEFAULT_CONFIG.published_message_ids_cache_time()
}

impl Default for Config {
    fn default() -> Self {
        let inner_default = DEFAULT_CONFIG.clone();
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
            duplicate_cache_time: inner_default.duplicate_cache_time(),
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
