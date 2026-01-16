use std::{net::Ipv4Addr, num::NonZeroU16, time::Duration};

use thiserror::Error;
use tracing::info;

use super::{
    connection::{PcpConnection, generate_nonce},
    mapping::Mapping,
    wire::{
        PCP_MAP_SIZE, PcpAnnounceRequest, PcpAnnounceResponse, PcpMapRequest, PcpMapResponse,
        Protocol, ResultCode,
    },
};

pub type IpPort = (Ipv4Addr, NonZeroU16);

#[derive(Debug, Error)]
pub enum PcpError {
    #[error("Network IO error: {0}")]
    Network(#[from] std::io::Error),
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
    #[error("Failed to parse response: {0}")]
    ParseError(String),
    #[error("Server returned error: {0:?}")]
    ServerError(ResultCode),
    #[error("Response nonce does not match request")]
    NonceMismatch,
    #[error("Response protocol does not match request")]
    ProtocolMismatch,
    #[error("Response port does not match request")]
    PortMismatch,
    #[error("Request timed out: {0}")]
    Timeout(#[source] tokio::time::error::Elapsed),
    #[error("Invalid response size: expected {expected} bytes, got {actual} bytes")]
    InvalidSize { expected: usize, actual: usize },
    #[error("Cannot get default gateway")]
    CannotGetGateway,
}

pub(super) const PCP_PORT: u16 = 5351;

#[derive(Debug, Clone)]
pub struct PcpConfig {
    /// Default mapping lifetime
    pub(super) mapping_lifetime: Duration,
    /// Initial retry delay
    pub(super) initial_retry: Duration,
    /// Maximum retry delay
    pub(super) max_retry_delay: Duration,
    /// Maximum number of retry attempts
    pub(super) max_retries: usize,
    /// Request timeout duration
    pub(super) request_timeout: Duration,
}

impl Default for PcpConfig {
    fn default() -> Self {
        Self {
            mapping_lifetime: Duration::from_secs(7200),
            initial_retry: Duration::from_millis(250),
            max_retry_delay: Duration::from_secs(1),
            max_retries: 5,
            request_timeout: Duration::from_secs(1),
        }
    }
}

/// PCP (RFC 6887) client for NAT port mapping.
///
/// Protocol flow:
/// 1. ANNOUNCE: Discovers PCP server and learns external IP
/// 2. MAP: Creates port mappings with nonce-based request/response matching
/// 3. DELETE: Removes mappings via MAP with 0-byte lifetime
///
/// All requests use server UDP port 5351 with exponential backoff retry.
/// Nonce prevent replay attacks and ensure response authenticity.
#[derive(Debug, Clone)]
pub struct PcpClient {
    client_addr: Ipv4Addr,
    config: PcpConfig,
}

/// Active PCP client that has confirmed gateway support and can create port
/// mappings.
#[derive(Debug, Clone)]
pub struct ActivePcpClient {
    client_addr: Ipv4Addr,
    gateway_ip: Ipv4Addr,
    config: PcpConfig,
}

impl PcpClient {
    /// Creates a new PCP client with the specified configuration.
    pub const fn new(client_addr: Ipv4Addr, config: PcpConfig) -> Self {
        Self {
            client_addr,
            config,
        }
    }

    /// Tests if a PCP server is available by sending an ANNOUNCE request.
    ///
    /// Returns an `ActivePcpClient` if the server responds successfully, which
    /// can then be used to create port mappings.
    ///
    /// Automatically discovers the default gateway for PCP communication.
    pub async fn probe_available(self) -> Result<ActivePcpClient, PcpError> {
        let gateway_ip = Self::get_default_gateway()?;

        tracing::debug!("Probing PCP support at gateway {gateway_ip}");

        let connection =
            PcpConnection::open(self.client_addr, gateway_ip, self.config.clone()).await?;

        let request = PcpAnnounceRequest::new(self.client_addr);

        // Discard the response - getting a valid PCP response proves the gateway
        // is reachable and speaks PCP protocol
        let _: PcpAnnounceResponse = connection.send_request(&request).await?;

        Ok(ActivePcpClient {
            client_addr: self.client_addr,
            gateway_ip,
            config: self.config,
        })
    }

    fn get_default_gateway() -> Result<Ipv4Addr, PcpError> {
        if let Ok(ipv4_addrs) = netdev::get_default_gateway().map(|g| g.ipv4)
            && let Some(gw) = ipv4_addrs.first()
        {
            return Ok(*gw);
        }

        Err(PcpError::CannotGetGateway)
    }
}

impl ActivePcpClient {
    /// Requests a port mapping from the PCP server.
    ///
    /// Creates a mapping for the specified protocol and internal port.
    /// Optionally accepts a preferred external IP and port.
    ///
    /// Returns the server-assigned external endpoint and mapping details.
    pub async fn request_mapping(
        &self,
        protocol: Protocol,
        internal_port: NonZeroU16,
        preferred_external: Option<IpPort>,
    ) -> Result<Mapping, PcpError> {
        let nonce = generate_nonce();

        let request = PcpMapRequest::new(
            self.client_addr,
            protocol,
            internal_port.get(),
            preferred_external.map(|(addr, port)| (addr, port.get())),
            self.config.mapping_lifetime.as_secs() as u32,
            nonce,
        );

        let connection =
            PcpConnection::open(self.client_addr, self.gateway_ip, self.config.clone()).await?;

        let response: PcpMapResponse = connection.send_request(&request).await?;

        let mapping = Mapping::from_map_response(
            &response,
            protocol,
            internal_port,
            nonce,
            self.client_addr,
            self.gateway_ip,
        )?;

        Self::log_mapping_success(&mapping);

        Ok(mapping)
    }

    fn log_mapping_success(mapping: &Mapping) {
        tracing::info!(
            "PCP mapping created: {}:{} (lifetime: {}s)",
            mapping.external_address(),
            mapping.external_port(),
            mapping.lifetime_seconds()
        );
    }

    /// Releases an existing port mapping by sending a DELETE request.
    ///
    /// Uses the mapping's original nonce to identify and delete the specific
    /// mapping.
    #[cfg(test)]
    pub(crate) async fn release_mapping(&self, mapping: Mapping) -> Result<(), PcpError> {
        info!(
            "Releasing PCP mapping: {}:{}",
            mapping.external_address(),
            mapping.external_port()
        );
        self.send_delete_request(mapping).await?;

        info!("PCP mapping released");
        Ok(())
    }

    #[cfg(test)]
    async fn send_delete_request(&self, mapping: Mapping) -> Result<(), PcpError> {
        let connection =
            PcpConnection::open(self.client_addr, self.gateway_ip, self.config.clone()).await?;

        let request = mapping.into_delete_request();

        let response: PcpMapResponse = connection.send_request(&request).await?;

        if response.payload.nonce != request.payload.nonce {
            return Err(PcpError::NonceMismatch);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with: `PCP_CLIENT_IP=192.168.1.100 cargo test
    /// test_request_port_mapping -- --ignored`
    #[tokio::test]
    #[ignore = "Integration test - requires gateway"]
    async fn test_request_port_mapping() {
        let client_ip = std::env::var("PCP_CLIENT_IP")
            .expect("PCP_CLIENT_IP environment variable required")
            .parse()
            .expect("PCP_CLIENT_IP must be valid IPv4 address");

        let client = PcpClient::new(client_ip, PcpConfig::default());

        match client.probe_available().await {
            Ok(active_client) => {
                println!("PCP server is available");

                println!("Requesting port mapping...");
                match active_client
                    .request_mapping(Protocol::Tcp, NonZeroU16::new(8080).unwrap(), None)
                    .await
                {
                    Ok(mapping) => {
                        println!(
                            "Got mapping: {}:{} (lifetime: {}s)",
                            mapping.external_address(),
                            mapping.external_port(),
                            mapping.lifetime_seconds()
                        );

                        match active_client.release_mapping(mapping).await {
                            Ok(()) => println!("Mapping released successfully"),
                            Err(e) => println!("Failed to release: {e}"),
                        }
                    }
                    Err(e) => {
                        println!("Port mapping failed: {e}");
                    }
                }
            }
            Err(e) => {
                println!("PCP server not available: {e}");
            }
        }
    }
}
