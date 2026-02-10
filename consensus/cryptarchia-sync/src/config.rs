use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The maximum duration to wait for a peer to respond
    /// with a message.
    pub peer_response_timeout: Duration,
}
