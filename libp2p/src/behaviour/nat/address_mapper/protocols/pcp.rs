use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::NonZeroU16,
};

use libp2p::Multiaddr;
use multiaddr::Protocol;
use tokio::time::timeout;
use tracing::info;

use crate::{
    behaviour::nat::address_mapper::{
        errors::AddressMapperError,
        protocols::pcp_core::{
            client::{PcpClient, PcpConfig},
            wire::Protocol as WireProtocol,
        },
    },
    config::NatMappingSettings,
};

pub struct PcpProtocol;

impl PcpProtocol {
    pub(crate) async fn map_address(
        address: &Multiaddr,
        settings: NatMappingSettings,
    ) -> Result<Multiaddr, AddressMapperError> {
        let (socket_addr, protocol) = parse_multiaddr(address)?;

        let local_ip = match socket_addr.ip() {
            IpAddr::V4(ip) => ip,
            IpAddr::V6(_) => {
                return Err(AddressMapperError::MultiaddrParseError(
                    "PCP only supports IPv4 addresses".to_owned(),
                ));
            }
        };

        let client = PcpClient::new(local_ip, PcpConfig::default());

        let active_client = timeout(settings.timeout, client.probe_available())
            .await
            .map_err(|_| {
                AddressMapperError::PortMappingFailed(
                    "Timeout waiting for PCP server availability check".to_owned(),
                )
            })?
            .map_err(|e| AddressMapperError::PortMappingFailed(e.to_string()))?;

        let internal_port = NonZeroU16::new(socket_addr.port()).ok_or_else(|| {
            AddressMapperError::MultiaddrParseError("Port cannot be zero".to_owned())
        })?;

        let mapping = timeout(
            settings.timeout,
            active_client.request_mapping(protocol, internal_port, None),
        )
        .await
        .map_err(|_| {
            AddressMapperError::PortMappingFailed(
                "Timeout waiting for PCP mapping response".to_owned(),
            )
        })?
        .map_err(|e| AddressMapperError::PortMappingFailed(e.to_string()))?;

        let external_addr = build_external_multiaddr(
            mapping.external_address(),
            mapping.external_port(),
            protocol,
        );

        info!(
            "PCP mapping created: {address} -> {external_addr} (lifetime: {}s)",
            mapping.lifetime_seconds()
        );

        Ok(external_addr)
    }
}

/// Parse multiaddr to extract socket address and protocol
fn parse_multiaddr(address: &Multiaddr) -> Result<(SocketAddr, WireProtocol), AddressMapperError> {
    let Some(ip) = address.iter().find_map(|protocol| match protocol {
        Protocol::Ip4(addr) => Some(IpAddr::V4(addr)),
        Protocol::Ip6(addr) => Some(IpAddr::V6(addr)),
        _ => None,
    }) else {
        return Err(AddressMapperError::NoIpAddress);
    };

    let Some((port, protocol)) = address.iter().find_map(|protocol| match protocol {
        Protocol::Tcp(port) => Some((port, WireProtocol::Tcp)),
        Protocol::Udp(port) => Some((port, WireProtocol::Udp)),
        _ => None,
    }) else {
        return Err(AddressMapperError::MultiaddrParseError(
            "No TCP or UDP port found in multiaddr".to_owned(),
        ));
    };

    if ip.is_ipv6() {
        return Err(AddressMapperError::MultiaddrParseError(
            "IPv6 not supported".to_owned(),
        ));
    }

    Ok((SocketAddr::new(ip, port), protocol))
}

/// Build external multiaddr from PCP mapping result
fn build_external_multiaddr(
    external_ip: Ipv4Addr,
    external_port: NonZeroU16,
    protocol: WireProtocol,
) -> Multiaddr {
    let mut addr = Multiaddr::empty();
    addr.push(Protocol::Ip4(external_ip));

    match protocol {
        WireProtocol::Tcp => addr.push(Protocol::Tcp(external_port.get())),
        WireProtocol::Udp => addr.push(Protocol::Udp(external_port.get())),
    }

    addr
}
