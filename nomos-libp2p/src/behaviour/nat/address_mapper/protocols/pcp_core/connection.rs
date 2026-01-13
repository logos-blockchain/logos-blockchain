use std::{
    fmt::Debug,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use backon::{ExponentialBuilder, Retryable as _};
use rand::RngCore as _;
use tokio::{
    net::UdpSocket,
    time::{Duration, timeout},
};
use zerocopy::{Immutable, IntoBytes};

use crate::behaviour::nat::address_mapper::protocols::pcp_core::client::{PcpConfig, PcpError};

const BUFFER_SIZE: usize = 1024;
/// Let OS choose any available port for client socket
const ANY_PORT: u16 = 0;

/// PCP connection for reliable communication with retry logic
///
/// Handles connection to PCP server with exponential backoff retry.
/// Each connection is bound to client IP and connected to server.
pub struct PcpConnection {
    socket: UdpSocket,
    config: PcpConfig,
}

impl PcpConnection {
    pub async fn open(
        client_addr: Ipv4Addr,
        gateway_ip: Ipv4Addr,
        config: PcpConfig,
    ) -> Result<Self, PcpError> {
        let socket = UdpSocket::bind((client_addr, ANY_PORT)).await?;

        let server_addr = SocketAddr::new(IpAddr::V4(gateway_ip), super::client::PCP_PORT);
        socket.connect(server_addr).await?;

        Ok(Self { socket, config })
    }

    pub(super) async fn send_request<Request, Response>(
        &self,
        request: &Request,
    ) -> Result<Response, PcpError>
    where
        Request: IntoBytes + Immutable + Sync,
        Response: for<'a> TryFrom<&'a [u8], Error: Debug>,
    {
        let backoff = create_backoff(
            self.config.initial_retry,
            self.config.max_retry_delay,
            self.config.max_retries,
        );

        (async || {
            self.socket.send(request.as_bytes()).await?;

            let mut buffer = vec![0u8; BUFFER_SIZE];

            match timeout(self.config.request_timeout, self.socket.recv(&mut buffer)).await {
                Ok(Ok(size)) => {
                    buffer.truncate(size);
                    Response::try_from(buffer.as_slice())
                        .map_err(|e| PcpError::InvalidResponse(format!("{e:?}")))
                }
                Ok(Err(e)) => Err(PcpError::from(e)),
                Err(elapsed) => Err(PcpError::Timeout(elapsed)),
            }
        })
        .retry(&backoff)
        .await
    }
}

/// Implements exponential backoff as specified in RFC 6887 Section 8.1.1:
/// "Clients should use exponential backoff with jitter to avoid thundering
/// herd"
fn create_backoff(
    initial_retry: Duration,
    max_delay: Duration,
    max_retries: usize,
) -> ExponentialBuilder {
    ExponentialBuilder::default()
        .with_min_delay(initial_retry)
        .with_max_delay(max_delay)
        .with_max_times(max_retries)
        .with_jitter()
}

/// Generate a random 12-byte nonce for PCP requests
pub fn generate_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    nonce
}
