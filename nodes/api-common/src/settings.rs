use std::{net::SocketAddr, str::FromStr, time::Duration};

use axum::http::HeaderValue;
use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};
use url::{Host, Url};

/// Configuration for the Http Server
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde_with::serde_as]
pub struct AxumBackendSettings {
    /// The chain this node runs on.
    ///
    /// Not user configurable: it is read off the genesis inscription of the
    /// deployment the node was built with, and handed to the backend so the
    /// API can serve it without asking a running service for a value that
    /// cannot change.
    pub chain_id: ChainId,
    /// The fork digest of every era of the deployment, era `n` at position
    /// `n`, from which the API serves the one in force.
    ///
    /// Not user configurable, like `chain_id`.
    pub fork_digests: Vec<ForkDigest>,
    /// Socket where the server will be listening on for incoming requests.
    pub address: SocketAddr,
    /// Browser origins allowed to use this server. A page from any other
    /// origin is refused.
    pub cors_origins: Vec<CorsOrigin>,
    /// Host names this server answers to, on top of `localhost` and any IP
    /// address. A request for any other name is refused.
    pub allowed_hosts: Vec<HostName>,
    /// Timeout for API requests in seconds (default: 30)
    #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    #[serde(default = "default_timeout")]
    pub timeout: Duration,
    /// Maximum request body size in bytes (default: 10MB)
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,
    /// Maximum number of concurrent requests
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: usize,
}

/// A browser origin, written as browsers send it: `https://app.example`, in
/// lowercase, with no path and no default port.
///
/// There is no wildcard, and no `null`: whoever reaches this server controls
/// the node, so each origin has to be named.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct CorsOrigin(String);

impl CorsOrigin {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn header_value(&self) -> HeaderValue {
        HeaderValue::from_str(&self.0).expect("A serialized origin is ASCII.")
    }
}

impl TryFrom<String> for CorsOrigin {
    type Error = String;

    fn try_from(origin: String) -> Result<Self, Self::Error> {
        match Url::parse(&origin) {
            Ok(url) if url.origin().ascii_serialization() == origin && !origin.contains('*') => {
                Ok(Self(origin))
            }
            _ => Err(format!(
                "The CORS origin {origin:?} must be one origin as a browser sends it, such as \
                 `https://app.example`: lowercase, with no path, no default port and no wildcard."
            )),
        }
    }
}

impl FromStr for CorsOrigin {
    type Err = String;

    fn from_str(origin: &str) -> Result<Self, Self::Err> {
        origin.to_owned().try_into()
    }
}

impl From<CorsOrigin> for String {
    fn from(origin: CorsOrigin) -> Self {
        origin.0
    }
}

/// A host name, such as `node.example`: lowercase, with no scheme and no port.
///
/// There is no wildcard, since answering to any name is what lets a web page
/// reach this server through DNS rebinding.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostName(String);

impl HostName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HostName {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        match Host::parse(&name) {
            Ok(Host::Domain(domain)) if domain == name && !name.contains('*') => Ok(Self(name)),
            _ => Err(format!(
                "The host name {name:?} must be a name such as `node.example`: lowercase, with no \
                 scheme, no port and no wildcard. IP addresses are always allowed."
            )),
        }
    }
}

impl FromStr for HostName {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        name.to_owned().try_into()
    }
}

impl From<HostName> for String {
    fn from(name: HostName) -> Self {
        name.0
    }
}

const fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

#[must_use]
pub const fn default_max_body_size() -> usize {
    // This has to allow for JSON object bytes, which is larger than the actual
    // payload data size. Typical JSON overhead for binary data:
    // - Base64 encoding: ~1.33× (33% increase).
    // - JSON structure (quotes, braces, commas): adds 10-20% typically.
    // - Combined realistic overhead: ~1.5-1.6× for Base64-encoded binary data in
    //   JSON.
    10 * 1024 * 1024
}

const fn default_max_concurrent_requests() -> usize {
    500
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_origin_accepts_what_a_browser_sends() {
        for origin in [
            "https://app.example",
            "http://localhost:3000",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(origin.parse::<CorsOrigin>().is_ok(), "{origin}");
        }
    }

    #[test]
    fn cors_origin_refuses_what_could_never_match_or_matches_too_much() {
        for origin in [
            "",
            "*",
            "null",
            "app.example",
            "localhost:3000",
            "https://*.app.example",
            "https://app.example/",
            "https://app.example/path",
            "https://APP.example",
            "https://app.example:443",
            "file:///index.html",
        ] {
            assert!(origin.parse::<CorsOrigin>().is_err(), "{origin}");
        }
    }

    #[test]
    fn host_name_accepts_a_name() {
        for name in [
            "node.example",
            "localhost",
            "logos-blockchain-node-0",
            "logos_blockchain_node_0",
        ] {
            assert!(name.parse::<HostName>().is_ok(), "{name}");
        }
    }

    #[test]
    fn host_name_refuses_what_could_never_match_or_matches_too_much() {
        for name in [
            "",
            "*",
            "*.node.example",
            "Node.example",
            "node.example:8080",
            "http://node.example",
            "127.0.0.1",
            "[::1]",
        ] {
            assert!(name.parse::<HostName>().is_err(), "{name}");
        }
    }
}
