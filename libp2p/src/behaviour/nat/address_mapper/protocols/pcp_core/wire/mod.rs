//! PCP wire format structures and constants.
//!
//! Zero-copy packet structures using `zerocopy` crate for parsing.
//! All multibyte fields use network byte order via `U16NE`/`U32NE` types.

pub mod announce;
pub mod map;

use std::net::{Ipv4Addr, Ipv6Addr};

pub(super) use announce::{
    AnnouncePayload, OPCODE_ANNOUNCE, PCP_ANNOUNCE_SIZE, PcpAnnounceRequest, PcpAnnounceResponse,
};
pub(super) use map::{
    MapPayload, MapResponsePayload, OPCODE_MAP, PCP_MAP_SIZE, PcpMapRequest, PcpMapResponse,
};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::network_endian::U32 as U32NE,
};

use crate::behaviour::nat::address_mapper::protocols::pcp_core::client::PcpError;

pub const PCP_VERSION: u8 = 2;
pub const IPV6_SIZE: usize = 16;

/// IPv4 address represented as IPv4-mapped IPv6 (`::ffff:a.b.c.d`)
///
/// PCP protocol uses this format to future-proof for IPv6 support.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct Ipv4MappedIpv6(pub [u8; IPV6_SIZE]);

impl Ipv4MappedIpv6 {
    pub const fn zero() -> Self {
        Self([0; IPV6_SIZE])
    }

    pub const fn as_bytes(&self) -> &[u8; IPV6_SIZE] {
        &self.0
    }
}

impl From<Ipv4Addr> for Ipv4MappedIpv6 {
    fn from(ipv4: Ipv4Addr) -> Self {
        let [a, b, c, d] = ipv4.octets();
        Self([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, a, b, c, d])
    }
}

impl TryFrom<Ipv4MappedIpv6> for Ipv4Addr {
    type Error = PcpError;

    fn try_from(value: Ipv4MappedIpv6) -> Result<Self, Self::Error> {
        let ipv6 = Ipv6Addr::from(value.0);
        ipv6.to_ipv4_mapped().ok_or_else(|| {
            Self::Error::InvalidResponse("Address is not IPv4-mapped IPv6 format".to_owned())
        })
    }
}

/// Transport protocol for port mappings (IANA protocol numbers)
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Protocol {
    /// TCP protocol (IANA 6)
    Tcp = 6,
    /// UDP protocol (IANA 17)
    Udp = 17,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum ResultCode {
    Success = 0,
    UnsupportedVersion = 1,
    NotAuthorized = 2,
    MalformedRequest = 3,
    UnsupportedOpcode = 4,
    UnsupportedOption = 5,
    MalformedOption = 6,
    NetworkFailure = 7,
    NoResources = 8,
    UnsupportedProtocol = 9,
    UserExceededQuota = 10,
    CannotProvideExternalAddress = 11,
    AddressMismatch = 12,
    ExcessiveRemotePeers = 13,
}

#[derive(Debug, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C)]
pub struct PcpRequestHeader {
    /// PCP protocol version (must be 2)
    pub version: u8,
    /// Operation code (0=ANNOUNCE, 1=MAP, etc.)
    pub opcode: u8,
    /// Reserved field (must be 0)
    pub reserved: [u8; 2],
    /// Requested lifetime in seconds
    pub lifetime: U32NE,
    /// Client IPv4 address as IPv4-mapped IPv6
    pub client_ip: Ipv4MappedIpv6,
}

impl PcpRequestHeader {
    fn new(client_addr: Ipv4Addr, opcode: u8, lifetime: u32) -> Self {
        Self {
            version: PCP_VERSION,
            opcode,
            reserved: [0; 2],
            lifetime: U32NE::new(lifetime),
            client_ip: Ipv4MappedIpv6::from(client_addr),
        }
    }
}

#[derive(Debug, FromBytes, IntoBytes, Unaligned, Immutable)]
#[repr(C)]
pub struct PcpRequest<Payload> {
    pub header: PcpRequestHeader,
    pub payload: Payload,
}

#[derive(Debug, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C)]
pub struct PcpResponseHeader {
    /// PCP protocol version (always 2)
    pub version: u8,
    /// Operation code with response bit (0x80 | opcode)
    pub opcode: u8,
    /// Result code (0=success, see `ResultCode` enum)
    pub result_code: u8,
    /// Reserved field
    pub reserved: u8,
    /// Server-assigned lifetime in seconds
    pub lifetime: U32NE,
    /// Server epoch time in seconds since boot
    pub epoch_time: U32NE,
    /// Reserved field (12 bytes)
    pub reserved2: [u8; 12],
}

impl PcpResponseHeader {
    /// Extracts the base opcode from the response opcode field.
    ///
    /// Response opcodes have the high bit (0x80) set, this method
    /// masks it out to get the original request opcode.
    pub const fn base_opcode(&self) -> u8 {
        self.opcode & 0x7F
    }

    /// Creates a response opcode from a request opcode by setting the response
    /// bit.
    pub const fn make_response_opcode(request_opcode: u8) -> u8 {
        request_opcode | 0x80
    }
}

#[derive(Debug, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C)]
pub struct PcpResponse<Payload> {
    pub header: PcpResponseHeader,
    pub payload: Payload,
}

impl<Payload> PcpRequest<Payload> {
    pub(super) fn build(
        client_addr: Ipv4Addr,
        opcode: u8,
        lifetime: u32,
        payload: Payload,
    ) -> Self {
        Self {
            header: PcpRequestHeader::new(client_addr, opcode, lifetime),
            payload,
        }
    }
}
