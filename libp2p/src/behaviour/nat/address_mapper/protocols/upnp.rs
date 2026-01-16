use std::net::{IpAddr, SocketAddr};

use igd_next::{
    PortMappingProtocol, SearchOptions,
    aio::{Gateway, tokio::Tokio},
};
use libp2p::Multiaddr;
use multiaddr::Protocol;
use tracing::info;

use crate::{
    behaviour::nat::address_mapper::errors::AddressMapperError, config::NatMappingSettings,
};

type AddressWithProtocol = (SocketAddr, PortMappingProtocol);

pub struct UpnpProtocol;

impl UpnpProtocol {
    async fn search_gateway_and_get_ip() -> Result<(Gateway<Tokio>, IpAddr), AddressMapperError> {
        let gateway = igd_next::aio::tokio::search_gateway(SearchOptions::default()).await?;
        let gateway_external_ip = gateway.get_external_ip().await?;

        info!("UPnP gateway found: {gateway_external_ip}");

        Ok((gateway, gateway_external_ip))
    }
}

impl UpnpProtocol {
    pub async fn map_address(
        address_to_map: &Multiaddr,
        settings: NatMappingSettings,
    ) -> Result<Multiaddr, AddressMapperError> {
        let (local_address, protocol) = multiaddr_to_socketaddr(address_to_map)?;
        let mapped_port = local_address.port();

        let (gateway, gateway_external_ip) = Self::search_gateway_and_get_ip().await?;

        gateway
            .add_port(
                protocol,
                // Request the same port as the local address
                mapped_port,
                local_address,
                settings.lease_duration.as_secs() as u32,
                "libp2p UPnP mapping",
            )
            .await?;

        let external_addr = external_address(gateway_external_ip, address_to_map);

        info!("Successfully added UPnP mapping: {external_addr}");

        Ok(external_addr)
    }
}

fn multiaddr_to_socketaddr(addr: &Multiaddr) -> Result<AddressWithProtocol, AddressMapperError> {
    let Some(ip) = addr.iter().find_map(|protocol| match protocol {
        Protocol::Ip4(addr) => Some(IpAddr::V4(addr)),
        Protocol::Ip6(addr) => Some(IpAddr::V6(addr)),
        _ => None,
    }) else {
        return Err(AddressMapperError::NoIpAddress);
    };

    let Some((port, protocol)) = addr.iter().find_map(|protocol| match protocol {
        Protocol::Tcp(port) => Some((port, PortMappingProtocol::TCP)),
        Protocol::Udp(port) => Some((port, PortMappingProtocol::UDP)),
        _ => None,
    }) else {
        return Err(AddressMapperError::MultiaddrParseError(
            "No TCP or UDP port found in multiaddr".to_owned(),
        ));
    };

    Ok((SocketAddr::new(ip, port), protocol))
}

/// Replace the IP in the Multiaddr with an external IP address.
/// Port is not changed.
fn external_address(external_address: IpAddr, local_address: &Multiaddr) -> Multiaddr {
    let addr = match external_address {
        IpAddr::V4(ip) => Protocol::Ip4(ip),
        IpAddr::V6(ip) => Protocol::Ip6(ip),
    };

    local_address
        .replace(0, |_| Some(addr))
        .expect("multiaddr should be valid")
}
