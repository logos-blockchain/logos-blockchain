use core::num::NonZeroU64;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub backend: BackendConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackendConfig {
    #[serde(default = "default_max_dial_attempts_per_peer_per_message")]
    pub max_dial_attempts_per_peer_per_message: NonZeroU64,
    // $\Phi_{EC}$: the minimum number of connections that the edge node establishes with
    // core nodes to send a single message that needs to be blended.
    #[serde(default = "default_replication_factor")]
    pub replication_factor: NonZeroU64,
}

const fn default_max_dial_attempts_per_peer_per_message() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
}

const fn default_replication_factor() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
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
