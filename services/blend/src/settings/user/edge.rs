use core::num::NonZeroU64;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub backend: BackendConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendConfig {
    pub max_dial_attempts_per_peer_per_message: NonZeroU64,
    // $\Phi_{EC}$: the minimum number of connections that the edge node establishes with
    // core nodes to send a single message that needs to be blended.
    pub replication_factor: NonZeroU64,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            max_dial_attempts_per_peer_per_message: NonZeroU64::new(1).unwrap(),
            replication_factor: NonZeroU64::new(1).unwrap(),
        }
    }
}
