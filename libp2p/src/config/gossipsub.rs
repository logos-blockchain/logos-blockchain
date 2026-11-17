use std::time::Duration;

use libp2p::gossipsub;
use serde::{Deserialize, Serialize};

// A partial copy of gossipsub::Config for deriving Serialize/Deserialize
// remotely https://serde.rs/remote-derive.html
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "gossipsub::Config")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Type matching `gossipsub::Config` for remote serde impl."
)]
pub struct ConfigDef {
    #[serde(getter = "gossipsub::Config::history_length")]
    history_length: usize,
    #[serde(getter = "gossipsub::Config::history_gossip")]
    history_gossip: usize,
    #[serde(getter = "gossipsub::Config::mesh_n")]
    mesh_n: usize,
    #[serde(getter = "gossipsub::Config::mesh_n_low")]
    mesh_n_low: usize,
    #[serde(getter = "gossipsub::Config::mesh_n_high")]
    mesh_n_high: usize,
    #[serde(getter = "gossipsub::Config::retain_scores")]
    retain_scores: usize,
    #[serde(getter = "gossipsub::Config::gossip_lazy")]
    gossip_lazy: usize,
    #[serde(getter = "gossipsub::Config::gossip_factor")]
    gossip_factor: f64,
    #[serde(getter = "gossipsub::Config::heartbeat_initial_delay")]
    heartbeat_initial_delay: Duration,
    #[serde(getter = "gossipsub::Config::heartbeat_interval")]
    heartbeat_interval: Duration,
    #[serde(getter = "gossipsub::Config::fanout_ttl")]
    fanout_ttl: Duration,
    #[serde(getter = "gossipsub::Config::check_explicit_peers_ticks")]
    check_explicit_peers_ticks: u64,
    #[serde(getter = "gossipsub::Config::duplicate_cache_time")]
    duplicate_cache_time: Duration,
    #[serde(getter = "gossipsub::Config::validate_messages")]
    validate_messages: bool,
    #[serde(getter = "gossipsub::Config::allow_self_origin")]
    allow_self_origin: bool,
    #[serde(getter = "gossipsub::Config::do_px")]
    do_px: bool,
    #[serde(getter = "gossipsub::Config::prune_peers")]
    prune_peers: usize,
    #[serde(getter = "gossipsub::Config::prune_backoff")]
    prune_backoff: Duration,
    #[serde(getter = "gossipsub::Config::unsubscribe_backoff")]
    unsubscribe_backoff: Duration,
    #[serde(getter = "gossipsub::Config::backoff_slack")]
    backoff_slack: u32,
    #[serde(getter = "gossipsub::Config::flood_publish")]
    flood_publish: bool,
    #[serde(getter = "gossipsub::Config::graft_flood_threshold")]
    graft_flood_threshold: Duration,
    #[serde(getter = "gossipsub::Config::mesh_outbound_min")]
    mesh_outbound_min: usize,
    #[serde(getter = "gossipsub::Config::opportunistic_graft_ticks")]
    opportunistic_graft_ticks: u64,
    #[serde(getter = "gossipsub::Config::opportunistic_graft_peers")]
    opportunistic_graft_peers: usize,
    #[serde(getter = "gossipsub::Config::gossip_retransimission")]
    gossip_retransimission: u32,
    #[serde(getter = "gossipsub::Config::max_publish_messages")]
    max_publish_messages: usize,
    #[serde(getter = "gossipsub::Config::max_control_messages_sent")]
    max_control_messages_sent: usize,
    #[serde(getter = "gossipsub::Config::max_control_message_size")]
    max_control_message_size: usize,
    #[serde(getter = "gossipsub::Config::max_ihave_messages_heartbeat")]
    max_ihave_messages_heartbeat: usize,
    #[serde(getter = "gossipsub::Config::iwant_followup_time")]
    iwant_followup_time: Duration,
}

#[expect(
    clippy::fallible_impl_from,
    reason = "`TryFrom` impl conflicting with blanket impl."
)]
impl From<ConfigDef> for gossipsub::Config {
    fn from(def: ConfigDef) -> Self {
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
            .unsubscribe_backoff(def.unsubscribe_backoff)
            .backoff_slack(def.backoff_slack)
            .flood_publish(def.flood_publish)
            .graft_flood_threshold(def.graft_flood_threshold)
            .mesh_outbound_min(def.mesh_outbound_min)
            .opportunistic_graft_ticks(def.opportunistic_graft_ticks)
            .opportunistic_graft_peers(def.opportunistic_graft_peers)
            .gossip_retransimission(def.gossip_retransimission)
            .max_publish_messages(def.max_publish_messages)
            .max_control_messages_sent(def.max_control_messages_sent)
            .max_control_message_size(def.max_control_message_size)
            .max_ihave_messages_heartbeat(def.max_ihave_messages_heartbeat)
            .iwant_followup_time(def.iwant_followup_time);

        if def.validate_messages {
            builder = builder.validate_messages();
        }
        if def.do_px {
            builder = builder.do_px();
        }

        builder.build().unwrap()
    }
}
