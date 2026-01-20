use std::net::Ipv4Addr;

use multiaddr::{Multiaddr, Protocol as MaProto};
use natpmp::{NatpmpAsync, Protocol, Response, new_tokio_natpmp};
use tokio::{net::UdpSocket, time::timeout};

use crate::{
    behaviour::nat::address_mapper::errors::AddressMapperError, config::NatMappingSettings,
};

type PortNumber = u16;

pub struct NatPmp;

impl NatPmp {
    async fn send_map_request(
        nat_pmp: &NatpmpAsync<UdpSocket>,
        protocol: Protocol,
        port: PortNumber,
        lease_duration: u32,
    ) -> Result<(), AddressMapperError> {
        nat_pmp
            .send_port_mapping_request(protocol, port, port, lease_duration)
            .await
            .map_err(|e| AddressMapperError::PortMappingFailed(e.to_string()))
    }

    async fn recv_map_response_public_port(
        nat_pmp: &NatpmpAsync<UdpSocket>,
    ) -> Result<PortNumber, AddressMapperError> {
        match nat_pmp
            .read_response_or_retry()
            .await
            .map_err(|e| AddressMapperError::PortMappingFailed(e.to_string()))?
        {
            Response::UDP(r) | Response::TCP(r) => Ok(r.public_port()),
            Response::Gateway(_) => Err(AddressMapperError::PortMappingFailed(
                "Expected UDP/TCP mapping response; got Gateway response".to_owned(),
            )),
        }
    }

    async fn query_public_ip(
        nat_pmp: &mut NatpmpAsync<UdpSocket>,
    ) -> Result<Ipv4Addr, AddressMapperError> {
        nat_pmp
            .send_public_address_request()
            .await
            .map_err(|e| AddressMapperError::GatewayDiscoveryFailed(e.to_string()))?;

        match nat_pmp
            .read_response_or_retry()
            .await
            .map_err(|e| AddressMapperError::GatewayDiscoveryFailed(e.to_string()))?
        {
            Response::Gateway(pa) => Ok(*pa.public_address()),
            other => Err(AddressMapperError::GatewayDiscoveryFailed(format!(
                "Expected PublicAddress response; got {other:?}"
            ))),
        }
    }
}

impl NatPmp {
    pub async fn map_address(
        local_address: &Multiaddr,
        settings: NatMappingSettings,
    ) -> Result<Multiaddr, AddressMapperError> {
        let (port, protocol) = extract_port_and_protocol(local_address)?;

        let mut nat_pmp = new_tokio_natpmp()
            .await
            .map_err(|e| AddressMapperError::PortMappingFailed(e.to_string()))?;

        Self::send_map_request(
            &nat_pmp,
            protocol,
            port,
            settings.lease_duration.as_secs() as u32,
        )
        .await?;

        let public_port = timeout(
            settings.timeout,
            Self::recv_map_response_public_port(&nat_pmp),
        )
        .await
        .map_err(|_| {
            AddressMapperError::PortMappingFailed(
                "Timeout waiting for NAT-PMP mapping response".to_owned(),
            )
        })??;

        let public_ip = timeout(settings.timeout, Self::query_public_ip(&mut nat_pmp))
            .await
            .map_err(|_| {
                AddressMapperError::PortMappingFailed(
                    "Timeout waiting for NAT-PMP public IP response".to_owned(),
                )
            })??;

        build_public_address(local_address, public_ip, public_port)
    }
}

fn extract_port_and_protocol(
    addr: &Multiaddr,
) -> Result<(PortNumber, Protocol), AddressMapperError> {
    addr.iter()
        .find_map(|p| match p {
            MaProto::Tcp(p) => Some((p, Protocol::TCP)),
            MaProto::Udp(p) => Some((p, Protocol::UDP)),
            _ => None,
        })
        .ok_or_else(|| {
            AddressMapperError::MultiaddrParseError(
                "No TCP or UDP port found in multiaddr".to_owned(),
            )
        })
}

fn build_public_address(
    local_address: &Multiaddr,
    public_ip: Ipv4Addr,
    public_port: PortNumber,
) -> Result<Multiaddr, AddressMapperError> {
    let with_ip = local_address
        .replace(0, |_| Some(multiaddr::Protocol::Ip4(public_ip)))
        .ok_or_else(|| {
            AddressMapperError::MultiaddrParseError("No IP address found in multiaddr".to_owned())
        })?;

    with_ip
        .replace(1, |p| match p {
            multiaddr::Protocol::Tcp(_) => Some(multiaddr::Protocol::Tcp(public_port)),
            multiaddr::Protocol::Udp(_) => Some(multiaddr::Protocol::Udp(public_port)),
            _ => None,
        })
        .ok_or_else(|| {
            AddressMapperError::MultiaddrParseError(
                "No TCP or UDP port found in multiaddr".to_owned(),
            )
        })
}
