use core::time::Duration;
use std::num::NonZeroUsize;

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
    /// The maximum number of inbound requests that can be handled concurrently,
    /// including requests waiting to be processed and requests currently being
    /// processed.
    pub max_inbound_requests: NonZeroUsize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_response_timeout: Duration::from_secs(5),
            max_inbound_requests: 10.try_into().unwrap(),
        }
    }
}
