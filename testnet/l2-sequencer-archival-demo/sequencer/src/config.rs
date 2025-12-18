use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// HTTP server listen address (e.g., "0.0.0.0:8080")
    pub listen_addr: SocketAddr,
    /// Nomos node HTTP endpoint to submit transactions to (e.g., "<http://localhost:18080>")
    pub node_endpoint: String,
    /// Path to the redb database file
    pub db_path: String,
    /// Path to the signing key file (will be created if it doesn't exist)
    pub signing_key_path: String,
    /// Channel ID for inscriptions (hex string, will be padded/truncated to 32
    /// bytes)
    pub channel_id: String,
    /// Initial balance for new accounts
    #[serde(default = "default_initial_balance")]
    pub initial_balance: u64,
    /// Basic auth username for node endpoint (optional)
    pub node_auth_username: Option<String>,
    /// Basic auth password for node endpoint (optional)
    pub node_auth_password: Option<String>,
}

const fn default_initial_balance() -> u64 {
    1000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8080".parse().expect("valid address"),
            node_endpoint: "http://localhost:18080".to_owned(),
            db_path: "sequencer.redb".to_owned(),
            signing_key_path: "sequencer.key".to_owned(),
            channel_id: String::new(),
            initial_balance: default_initial_balance(),
            node_auth_username: None,
            node_auth_password: None,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            listen_addr: std::env::var("SEQUENCER_LISTEN_ADDR")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| "0.0.0.0:8080".parse().unwrap()),
            node_endpoint: std::env::var("SEQUENCER_NODE_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:18080".to_owned()),
            db_path: std::env::var("SEQUENCER_DB_PATH")
                .unwrap_or_else(|_| "sequencer.redb".to_owned()),
            signing_key_path: std::env::var("SEQUENCER_SIGNING_KEY_PATH")
                .unwrap_or_else(|_| "sequencer.key".to_owned()),
            channel_id: std::env::var("SEQUENCER_CHANNEL_ID")
                .expect("SEQUENCER_CHANNEL_ID env var is required"),
            initial_balance: std::env::var("SEQUENCER_INITIAL_BALANCE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_initial_balance),
            node_auth_username: std::env::var("SEQUENCER_NODE_AUTH_USERNAME").ok(),
            node_auth_password: std::env::var("SEQUENCER_NODE_AUTH_PASSWORD").ok(),
        }
    }
}
