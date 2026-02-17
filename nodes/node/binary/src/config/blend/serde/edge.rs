#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use crate::config::utils;

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub backend: BackendConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BackendConfig {
    #[serde(skip_serializing_if = "is_default_max_dial_attempts_per_peer_per_message")]
    pub max_dial_attempts_per_peer_per_message: NonZeroU64,
    // $\Phi_{EC}$: the minimum number of connections that the edge node establishes with
    // core nodes to send a single message that needs to be blended.
    #[serde(skip_serializing_if = "is_default_replication_factor")]
    pub replication_factor: NonZeroU64,
}

const fn default_max_dial_attempts_per_peer_per_message() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
}

fn is_default_max_dial_attempts_per_peer_per_message(value: &NonZeroU64) -> bool {
    *value == default_max_dial_attempts_per_peer_per_message()
}

const fn default_replication_factor() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
}

fn is_default_replication_factor(value: &NonZeroU64) -> bool {
    *value == default_replication_factor()
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            max_dial_attempts_per_peer_per_message: default_max_dial_attempts_per_peer_per_message(
            ),
            replication_factor: default_replication_factor(),
        }
    }
}
