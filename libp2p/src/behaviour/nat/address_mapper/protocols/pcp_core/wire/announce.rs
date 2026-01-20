use std::net::Ipv4Addr;

use zerocopy::{FromBytes, FromZeros as _, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::{PCP_VERSION, PcpRequest, PcpResponse, PcpResponseHeader, ResultCode};
use crate::behaviour::nat::address_mapper::protocols::pcp_core::client::PcpError;

pub const OPCODE_ANNOUNCE: u8 = 0;
pub const PCP_ANNOUNCE_SIZE: usize = 24;

pub type PcpAnnounceRequest = PcpRequest<AnnouncePayload>;

pub type PcpAnnounceResponse = PcpResponse<AnnouncePayload>;

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C)]
pub struct AnnouncePayload;

impl PcpAnnounceRequest {
    pub fn new(client_addr: Ipv4Addr) -> Self {
        Self::build(client_addr, OPCODE_ANNOUNCE, 0, AnnouncePayload)
    }
}

impl<'a> TryFrom<&'a [u8]> for PcpAnnounceResponse {
    type Error = PcpError;

    fn try_from(data: &'a [u8]) -> Result<Self, Self::Error> {
        if data.len() != PCP_ANNOUNCE_SIZE {
            return Err(PcpError::InvalidSize {
                expected: PCP_ANNOUNCE_SIZE,
                actual: data.len(),
            });
        }

        let response = Self::read_from_bytes(data)
            .map_err(|e| PcpError::ParseError(format!("Failed to parse ANNOUNCE response: {e}")))?;

        if response.header.version != PCP_VERSION {
            return Err(PcpError::InvalidResponse(format!(
                "Version mismatch: expected {}, got {}",
                PCP_VERSION, response.header.version
            )));
        }

        if response.header.base_opcode() != OPCODE_ANNOUNCE {
            return Err(PcpError::InvalidResponse(format!(
                "Unexpected opcode in response: expected {OPCODE_ANNOUNCE}, got {}",
                response.header.base_opcode()
            )));
        }

        let Ok(result_code) = ResultCode::try_from(response.header.result_code) else {
            tracing::warn!(
                "Unknown PCP result code in ANNOUNCE response: {}",
                response.header.result_code
            );

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
    fn test_pcp_announce_request_zero_lifetime() {
        let client_ip = Ipv4Addr::new(192, 168, 1, 100);
        let request = PcpAnnounceRequest::new(client_ip);

        assert_eq!(request.header.lifetime.get(), 0);
        assert_eq!(request.header.opcode, OPCODE_ANNOUNCE);
        assert_eq!(request.header.version, PCP_VERSION);
    }

    #[test]
    fn test_pcp_announce_response_validation() {
        let mut response = PcpAnnounceResponse::new_zeroed();
        response.header.version = PCP_VERSION;
        response.header.opcode = PcpResponseHeader::make_response_opcode(OPCODE_ANNOUNCE);
        response.header.result_code = ResultCode::Success as u8;
        response.header.lifetime.set(7200);
        response.header.epoch_time.set(1_234_567_890);

        let data = response.as_bytes();
        assert!(PcpAnnounceResponse::try_from(data).is_ok());

        let short_data = &data[..10];
        assert!(matches!(
            PcpAnnounceResponse::try_from(short_data),
            Err(PcpError::InvalidSize { .. })
        ));

        let mut bad_version = PcpAnnounceResponse::new_zeroed();
        bad_version.header.version = 1;
        bad_version.header.opcode = PcpResponseHeader::make_response_opcode(OPCODE_ANNOUNCE);
        bad_version.header.result_code = ResultCode::Success as u8;
        assert!(matches!(
            PcpAnnounceResponse::try_from(bad_version.as_bytes()),
            Err(PcpError::InvalidResponse(_))
        ));

        let mut bad_opcode = PcpAnnounceResponse::new_zeroed();
        bad_opcode.header.version = PCP_VERSION;
        bad_opcode.header.opcode = PcpResponseHeader::make_response_opcode(1);
        bad_opcode.header.result_code = ResultCode::Success as u8;
        assert!(matches!(
            PcpAnnounceResponse::try_from(bad_opcode.as_bytes()),
            Err(PcpError::InvalidResponse(_))
        ));

        let mut server_error = PcpAnnounceResponse::new_zeroed();
        server_error.header.version = PCP_VERSION;
        server_error.header.opcode = PcpResponseHeader::make_response_opcode(OPCODE_ANNOUNCE);
        server_error.header.result_code = ResultCode::NetworkFailure as u8;
        assert!(matches!(
            PcpAnnounceResponse::try_from(server_error.as_bytes()),
            Err(PcpError::ServerError(ResultCode::NetworkFailure))
        ));
    }
}
