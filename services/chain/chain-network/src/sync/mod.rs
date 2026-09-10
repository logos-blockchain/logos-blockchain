pub mod config;
pub mod orphan_handler;
pub mod rejected_blocks;
pub mod tip_poll;

use lb_log_targets::chain;

const LOG_TARGET: &str = chain::network::SYNC;
