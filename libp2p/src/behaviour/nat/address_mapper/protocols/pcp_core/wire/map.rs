use std::net::Ipv4Addr;

use zerocopy::{
    FromBytes, FromZeros as _, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::network_endian::{U16 as U16NE, U32 as U32NE},
};

use super::{Ipv4MappedIpv6, PCP_VERSION, PcpRequest, PcpResponse, Protocol, ResultCode};
use crate::behaviour::nat::address_mapper::protocols::pcp_core::client::PcpError;

pub const OPCODE_MAP: u8 = 1;
pub const PCP_MAP_SIZE: usize = 60;

pub type PcpMapRequest = PcpRequest<MapPayload>;

pub type PcpMapResponse = PcpResponse<MapResponsePayload>;

#[derive(Debug, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C)]
pub struct MapPayload {
    /// Random 96-bit nonce for request/response matching
    pub nonce: [u8; 12],
    /// Transport protocol (6=TCP, 17=UDP)
    pub protocol: u8,
    /// Reserved field (must be 0)
    pub reserved: [u8; 3],
    /// Internal (private) port number
    pub internal_port: U16NE,
    /// Suggested external port (0=any)
    pub external_port: U16NE,
    /// Suggested external IPv4 as IPv4-mapped IPv6 (0=any)
    pub external_address: Ipv4MappedIpv6,
}

impl MapPayload {
    pub(super) fn new(
        nonce: [u8; 12],
        protocol: Protocol,
        internal_port: u16,
        preferred_external: Option<(Ipv4Addr, u16)>,
    ) -> Self {
        let (external_port, external_address) = match preferred_external {
            Some((addr, port)) => (port, Ipv4MappedIpv6::from(addr)),
            None => (0, Ipv4MappedIpv6::zero()),
        };

        Self {
            nonce,
            protocol: protocol as u8,
            reserved: [0; 3],
            internal_port: U16NE::new(internal_port),
            external_port: U16NE::new(external_port),
            external_address,
        }
    }
}

#[derive(Debug, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C)]
pub struct MapResponsePayload {
    /// Nonce from request (must match for validation)
    pub nonce: [u8; 12],
    /// Transport protocol from request
    pub protocol: u8,
    /// Reserved field
    pub reserved: [u8; 3],
    /// Internal port from request
    pub internal_port: U16NE,
    /// Server-assigned external port number
    pub assigned_external_port: U16NE,
    /// Server-assigned external IPv4 as IPv4-mapped IPv6
    pub assigned_external_address: Ipv4MappedIpv6,
}

impl PcpMapRequest {
    pub fn new(
        client_addr: Ipv4Addr,
        protocol: Protocol,
        internal_port: u16,
        preferred_external: Option<(Ipv4Addr, u16)>,
        lifetime_seconds: u32,
        nonce: [u8; 12],
    ) -> Self {
        let payload = MapPayload::new(nonce, protocol, internal_port, preferred_external);
        Self::build(client_addr, OPCODE_MAP, lifetime_seconds, payload)
    }

    #[cfg(test)]
    pub(crate) fn new_delete(
        client_addr: Ipv4Addr,
        protocol: Protocol,
        internal_port: u16,
        nonce: [u8; 12],
    ) -> Self {
        let payload = MapPayload::new(nonce, protocol, internal_port, None);
        Self::build(client_addr, OPCODE_MAP, 0, payload)
    }
}

impl TryFrom<&[u8]> for PcpMapResponse {
    type Error = PcpError;

    fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
        if data.len() != PCP_MAP_SIZE {
            return Err(PcpError::InvalidSize {
                expected: PCP_MAP_SIZE,
                actual: data.len(),
            });
        }

        let response = Self::read_from_bytes(data)
            .map_err(|e| PcpError::ParseError(format!("Failed to parse MAP response: {e}")))?;

        let Ok(result_code) = ResultCode::try_from(response.header.result_code) else {
            tracing::warn!("Unknown PCP result code: {}", response.header.result_code);

            return Err(PcpError::InvalidResponse(format!(
                "Unknown result code: {}",
                response.header.result_code
            )));
        };

        if result_code != ResultCode::Success {
            return Err(PcpError::ServerError(result_code));
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use zerocopy::{FromZeros as _, IntoBytes as _};

    use super::*;

    #[test]
    fn test_pcp_map_request_creation() {
        let client_ip = Ipv4Addr::new(192, 168, 1, 100);
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let preferred_external = Some((Ipv4Addr::new(203, 0, 113, 1), 8080));

        let request = PcpMapRequest::new(
            client_ip,
            Protocol::Tcp,
            8080,
            preferred_external,
            7200,
            nonce,
        );

        assert_eq!(request.header.version, PCP_VERSION);
        assert_eq!(request.header.opcode, OPCODE_MAP);
        assert_eq!(request.header.lifetime.get(), 7200);
        assert_eq!(request.header.client_ip, Ipv4MappedIpv6::from(client_ip));
        assert_eq!(request.payload.nonce, nonce);
        assert_eq!(request.payload.protocol, Protocol::Tcp as u8);
        assert_eq!(request.payload.internal_port.get(), 8080);
        assert_eq!(request.payload.external_port.get(), 8080);
        assert_eq!(
            request.payload.external_address,
            Ipv4MappedIpv6::from(Ipv4Addr::new(203, 0, 113, 1))
        );
    }

    #[test]
    fn test_pcp_map_request_no_preferred() {
        let client_ip = Ipv4Addr::new(192, 168, 1, 100);
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

        let request = PcpMapRequest::new(client_ip, Protocol::Udp, 9000, None, 3600, nonce);

        assert_eq!(request.header.version, PCP_VERSION);
        assert_eq!(request.header.opcode, OPCODE_MAP);
        assert_eq!(request.header.lifetime.get(), 3600);
        assert_eq!(request.payload.protocol, Protocol::Udp as u8);
        assert_eq!(request.payload.internal_port.get(), 9000);
        assert_eq!(request.payload.external_port.get(), 0);
        assert_eq!(request.payload.external_address, Ipv4MappedIpv6::zero());
    }

    #[test]
    fn test_pcp_map_request_delete() {
        let client_ip = Ipv4Addr::new(192, 168, 1, 100);
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

        let request = PcpMapRequest::new_delete(client_ip, Protocol::Tcp, 8080, nonce);

        assert_eq!(request.header.version, PCP_VERSION);
        assert_eq!(request.header.opcode, OPCODE_MAP);
        assert_eq!(request.header.lifetime.get(), 0);
        assert_eq!(request.header.client_ip, Ipv4MappedIpv6::from(client_ip));
        assert_eq!(request.payload.nonce, nonce);
        assert_eq!(request.payload.protocol, Protocol::Tcp as u8);
        assert_eq!(request.payload.internal_port.get(), 8080);
    }
}
