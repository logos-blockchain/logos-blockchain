use std::{num::NonZeroUsize, time::Duration};

use serde::{Deserialize, Serialize};
use serde_with::{DurationMilliSeconds, serde_as};

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The maximum duration to wait for a peer to respond
    /// with a message.
    #[serde_as(as = "DurationMilliSeconds<u64>")]
    pub peer_response_timeout: Duration,
    /// The maximum number of inbound block download requests that can be
    /// served concurrently. The same limit separately applies to inbound
    /// requests whose message is still being read. Tip requests are not
    /// counted against the download limit.
    pub max_inbound_requests: NonZeroUsize,
}
