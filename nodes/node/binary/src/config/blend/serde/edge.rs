use core::num::NonZeroU64;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: BackendConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackendConfig {
    pub max_dial_attempts_per_peer_per_message: NonZeroU64,
    // $\Phi_{EC}$: the minimum number of connections that the edge node establishes with
    // core nodes to send a single message that needs to be blended.
    pub replication_factor: NonZeroU64,
}
