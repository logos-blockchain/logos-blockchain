use std::{net::Ipv4Addr, num::NonZeroU16};

use zerocopy::FromBytes as _;

use crate::behaviour::nat::address_mapper::protocols::pcp_core::{
    client::PcpError,
    wire::{
        Ipv4MappedIpv6, MapPayload, OPCODE_MAP, PCP_MAP_SIZE, PcpMapRequest, PcpMapResponse,
        Protocol, ResultCode,
    },
};

#[derive(Debug)]
pub struct Mapping {
    protocol: Protocol,
    local_ip: Ipv4Addr,
    local_port: NonZeroU16,
    gateway: Ipv4Addr,
    external_address: Ipv4Addr,
    external_port: NonZeroU16,
    lifetime_seconds: u32,
    nonce: [u8; 12],
    epoch_time: u32,
}

impl Mapping {
    /// Returns the external (public) IP address of this mapping
    pub const fn external_address(&self) -> Ipv4Addr {
        self.external_address
    }

    /// Returns the external (public) port of this mapping
    pub const fn external_port(&self) -> NonZeroU16 {
        self.external_port
    }

    /// Returns the remaining lifetime of this mapping in seconds
    pub const fn lifetime_seconds(&self) -> u32 {
        self.lifetime_seconds
    }

    pub(super) fn from_map_response(
        response: &PcpMapResponse,
        expected_protocol: Protocol,
        expected_port: NonZeroU16,
        expected_nonce: [u8; 12],
        local_ip: Ipv4Addr,
        gateway: Ipv4Addr,
    ) -> Result<Self, PcpError> {
        if response.payload.nonce != expected_nonce {
            return Err(PcpError::NonceMismatch);
        }

        if response.payload.protocol != expected_protocol as u8 {
            return Err(PcpError::ProtocolMismatch);
        }

        if response.payload.internal_port.get() != expected_port.get() {
            return Err(PcpError::PortMismatch);
        }

        let external_v4 = Ipv4Addr::try_from(response.payload.assigned_external_address)?;

        let assigned_external_port = NonZeroU16::new(response.payload.assigned_external_port.get())
            .ok_or_else(|| {
                PcpError::InvalidResponse(format!(
                    "Invalid external port: {}",
                    response.payload.assigned_external_port.get()
                ))
            })?;

        Ok(Self {
            protocol: expected_protocol,
            local_ip,
            local_port: expected_port,
            gateway,
            external_port: assigned_external_port,
            external_address: external_v4,
            lifetime_seconds: response.header.lifetime.get(),
            nonce: response.payload.nonce,
            epoch_time: response.header.epoch_time.get(),
        })
    }

    #[cfg(test)]
    pub(crate) fn into_delete_request(self) -> PcpMapRequest {
        PcpMapRequest::new_delete(
            self.local_ip,
            self.protocol,
            self.local_port.get(),
            self.nonce,
        )
    }
}

#[cfg(test)]
mod tests {
    use zerocopy::{FromZeros as _, IntoBytes as _};

    use super::*;
    use crate::behaviour::nat::address_mapper::protocols::pcp_core::wire::{
        PCP_VERSION, PcpResponseHeader,
    };

    fn create_valid_map_response(
        nonce: [u8; 12],
        protocol: u8,
        internal_port: u16,
        external_port: u16,
        external_ip: Ipv4Addr,
        lifetime: u32,
        result_code: u8,
    ) -> Vec<u8> {
        let mut response = PcpMapResponse::new_zeroed();
        response.header.version = PCP_VERSION;
        response.header.opcode = PcpResponseHeader::make_response_opcode(OPCODE_MAP);
        response.header.result_code = result_code;
        response.header.lifetime.set(lifetime);
        response.header.epoch_time.set(1_234_567_890);
        response.payload.nonce = nonce;
        response.payload.protocol = protocol;
        response.payload.internal_port.set(internal_port);
        response.payload.assigned_external_port.set(external_port);
        response.payload.assigned_external_address = Ipv4MappedIpv6::from(external_ip);
        response.as_bytes().to_vec()
    }

    #[test]
    fn test_mapping_from_valid_response() {
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let local_ip = Ipv4Addr::new(192, 168, 1, 100);
        let gateway = Ipv4Addr::new(192, 168, 1, 1);
        let external_ip = Ipv4Addr::new(203, 0, 113, 50);
        let internal_port = NonZeroU16::new(8080).unwrap();
        let external_port = 8081;

        let response_data = create_valid_map_response(
            nonce,
            Protocol::Tcp as u8,
            internal_port.get(),
            external_port,
            external_ip,
            7200,
            ResultCode::Success as u8,
        );

        let response = PcpMapResponse::try_from(response_data.as_slice()).unwrap();
        let mapping = Mapping::from_map_response(
            &response,
            Protocol::Tcp,
            internal_port,
            nonce,
            local_ip,
            gateway,
        )
        .unwrap();

        assert_eq!(mapping.protocol, Protocol::Tcp);
        assert_eq!(mapping.local_ip, local_ip);
        assert_eq!(mapping.local_port, internal_port);
        assert_eq!(mapping.gateway, gateway);
        assert_eq!(mapping.external_port().get(), external_port);
        assert_eq!(mapping.external_address(), external_ip);
        assert_eq!(mapping.lifetime_seconds(), 7200);
        assert_eq!(mapping.nonce, nonce);
        assert_eq!(mapping.epoch_time, 1_234_567_890);
    }

    #[test]
    fn test_mapping_invalid_size() {
        let short_data = vec![0u8; 10];
        let result = PcpMapResponse::try_from(short_data.as_slice());

        assert!(matches!(
            result,
            Err(PcpError::InvalidSize {
                expected: PCP_MAP_SIZE,
                actual: 10
            })
        ));

        let long_data = vec![0u8; PCP_MAP_SIZE + 10];
        let result = PcpMapResponse::try_from(long_data.as_slice());

        assert!(matches!(
            result,
            Err(PcpError::InvalidSize {
                expected: PCP_MAP_SIZE,
                actual: 70
            })
        ));
    }

    fn assert_mapping_error(
        response_nonce: [u8; 12],
        response_protocol: u8,
        response_internal_port: u16,
        expected_nonce: [u8; 12],
        expected_protocol: Protocol,
        expected_port: NonZeroU16,
        expected_error: impl Fn(&PcpError) -> bool,
    ) {
        let response_data = create_valid_map_response(
            response_nonce,
            response_protocol,
            response_internal_port,
            8081,
            Ipv4Addr::new(203, 0, 113, 50),
            7200,
            ResultCode::Success as u8,
        );
        let response = PcpMapResponse::try_from(response_data.as_slice()).unwrap();
        let result = Mapping::from_map_response(
            &response,
            expected_protocol,
            expected_port,
            expected_nonce,
            Ipv4Addr::new(192, 168, 1, 100),
            Ipv4Addr::new(192, 168, 1, 1),
        );
        assert!(result.is_err());
        assert!(expected_error(&result.unwrap_err()));
    }

    #[test]
    fn test_mapping_validation_errors() {
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let wrong_nonce = [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1];
        let port = NonZeroU16::new(8080).unwrap();

        assert_mapping_error(
            wrong_nonce,
            Protocol::Tcp as u8,
            8080,
            nonce,
            Protocol::Tcp,
            port,
            |e| matches!(e, PcpError::NonceMismatch),
        );

        assert_mapping_error(
            nonce,
            Protocol::Udp as u8,
            8080,
            nonce,
            Protocol::Tcp,
            port,
            |e| matches!(e, PcpError::ProtocolMismatch),
        );

        assert_mapping_error(
            nonce,
            Protocol::Tcp as u8,
            9000,
            nonce,
            Protocol::Tcp,
            port,
            |e| matches!(e, PcpError::PortMismatch),
        );

        let mut response = PcpMapResponse::new_zeroed();
        response.header.version = PCP_VERSION;
        response.header.opcode = PcpResponseHeader::make_response_opcode(OPCODE_MAP);
        response.header.result_code = ResultCode::Success as u8;
        response.header.lifetime.set(7200);
        response.payload.nonce = nonce;
        response.payload.protocol = Protocol::Tcp as u8;
        response.payload.internal_port.set(8080);
        response.payload.assigned_external_port.set(8081);
        response.payload.assigned_external_address =
            Ipv4MappedIpv6([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

        let result = Mapping::from_map_response(
            &response,
            Protocol::Tcp,
            NonZeroU16::new(8080).unwrap(),
            nonce,
            Ipv4Addr::new(192, 168, 1, 100),
            Ipv4Addr::new(192, 168, 1, 1),
        );

        assert!(
            matches!(result, Err(PcpError::InvalidResponse(msg)) if msg.contains("Address is not IPv4-mapped"))
        );

        let response_data = create_valid_map_response(
            nonce,
            Protocol::Tcp as u8,
            8080,
            0,
            Ipv4Addr::new(203, 0, 113, 50),
            7200,
            ResultCode::Success as u8,
        );

        let response = PcpMapResponse::try_from(response_data.as_slice()).unwrap();
        let result = Mapping::from_map_response(
            &response,
            Protocol::Tcp,
            NonZeroU16::new(8080).unwrap(),
            nonce,
            Ipv4Addr::new(192, 168, 1, 100),
            Ipv4Addr::new(192, 168, 1, 1),
        );

        assert!(
            matches!(result, Err(PcpError::InvalidResponse(msg)) if msg.contains("Invalid external port: 0"))
        );
    }

    #[test]
    fn test_mapping_server_error() {
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

        let error_codes = [
            ResultCode::NetworkFailure,
            ResultCode::NoResources,
            ResultCode::UnsupportedProtocol,
        ];

        for error_code in error_codes {
            let response_data = create_valid_map_response(
                nonce,
                Protocol::Tcp as u8,
                8080,
                8081,
                Ipv4Addr::new(203, 0, 113, 50),
                7200,
                error_code as u8,
            );

            let response = PcpMapResponse::try_from(response_data.as_slice()).unwrap_err();
            assert!(matches!(response, PcpError::ServerError(err) if err == error_code));
        }
    }

    #[test]
    fn test_mapping_unknown_result_code() {
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

        let response_data = create_valid_map_response(
            nonce,
            Protocol::Tcp as u8,
            8080,
            8081,
            Ipv4Addr::new(203, 0, 113, 50),
            7200,
            99,
        );

        let result = PcpMapResponse::try_from(response_data.as_slice());
        assert!(matches!(result, Err(PcpError::InvalidResponse(_))));
    }

    #[test]
    fn test_mapping_malformed_response_data() {
        let malformed_data = vec![0xFF; PCP_MAP_SIZE];

        let result = PcpMapResponse::try_from(malformed_data.as_slice());
        assert!(result.is_err());
    }
}
