use core::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::{DurationMilliSeconds, serde_as};

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The maximum duration to wait for a peer to respond
    /// with a message.
    #[serde_as(as = "DurationMilliSeconds<u64>")]
    pub peer_response_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_response_timeout: Duration::from_secs(5),
        }
    }
}
