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
    /// The maximum number of inbound block download requests that can be
    /// served concurrently. The same limit separately applies to inbound
    /// requests whose message is still being read. Tip requests are not
    /// counted against the download limit.
    pub max_inbound_requests: NonZeroUsize,
    /// The maximum duration to serve one inbound block download stream, from
    /// receiving the request until the stream is closed. Unlike
    /// `peer_response_timeout`, which bounds each message, this bounds the
    /// whole download, so a peer that keeps reading slowly cannot hold a
    /// download slot indefinitely.
    #[serde_as(as = "DurationMilliSeconds<u64>")]
    pub inbound_download_stream_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_response_timeout: Duration::from_secs(5),
            max_inbound_requests: 10.try_into().unwrap(),
            inbound_download_stream_timeout: Duration::from_secs(120),
        }
    }
}
