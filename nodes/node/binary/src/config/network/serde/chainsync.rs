use core::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::{DurationMilliSeconds, serde_as};

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    /// The maximum duration to wait for a peer to respond
    /// with a message.
    #[serde_as(as = "DurationMilliSeconds<u64>")]
    #[serde(skip_serializing_if = "is_default_peer_response_timeout")]
    pub peer_response_timeout: Duration,
}

const fn default_peer_response_timeout() -> Duration {
    Duration::from_secs(5)
}

fn is_default_peer_response_timeout(timeout: &Duration) -> bool {
    *timeout == default_peer_response_timeout()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_response_timeout: default_peer_response_timeout(),
        }
    }
}
