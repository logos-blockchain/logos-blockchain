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
    /// The largest response that carries a block: one that carries the largest
    /// block of any era.
    pub max_block_response_size: usize,
    /// The maximum duration to serve one inbound block download stream, from
    /// receiving the request until the stream is closed. Unlike
    /// `peer_response_timeout`, which bounds each message, this bounds the
    /// whole download, so a peer that keeps reading slowly cannot hold a
    /// download slot indefinitely.
    #[serde_as(as = "DurationMilliSeconds<u64>")]
    pub inbound_download_stream_timeout: Duration,
}
