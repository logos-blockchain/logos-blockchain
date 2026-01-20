use libp2p::{
    Multiaddr, autonat,
    swarm::{FromSwarm, NewListenAddr, behaviour::ExternalAddrConfirmed},
};

use crate::behaviour::nat::address_mapper;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    AutonatClientTestFailed(Multiaddr),
    AutonatClientTestOk(Multiaddr),
    AddressMappingFailed(Multiaddr),
    DefaultGatewayChanged {
        /// Previous gateway address
        old_gateway: Option<std::net::IpAddr>,
        /// New gateway address
        new_gateway: std::net::IpAddr,
        /// Local address that needs to be re-mapped (if available)
        local_address: Option<Multiaddr>,
    },
    ExternalAddressConfirmed(Multiaddr),
    LocalAddressChanged(Multiaddr),
    NewListenAddress(Multiaddr),
    NewExternalMappedAddress {
        local_address: Multiaddr,
        external_address: Multiaddr,
    },
}

impl TryFrom<&autonat::v2::client::Event> for Event {
    type Error = ();

    fn try_from(event: &autonat::v2::client::Event) -> Result<Self, Self::Error> {
        Ok(match event {
            autonat::v2::client::Event {
                result: Err(_),
                tested_addr,
                ..
            } => Self::AutonatClientTestFailed(tested_addr.clone()),
            autonat::v2::client::Event {
                result: Ok(()),
                tested_addr,
                ..
            } => Self::AutonatClientTestOk(tested_addr.clone()),
        })
    }
}

impl TryFrom<&address_mapper::Event> for Event {
    type Error = ();

    fn try_from(event: &address_mapper::Event) -> Result<Self, Self::Error> {
        Ok(match event {
            address_mapper::Event::AddressMappingFailed(addr) => {
                Self::AddressMappingFailed(addr.clone())
            }
            address_mapper::Event::DefaultGatewayChanged {
                old_gateway,
                new_gateway,
                local_address,
            } => Self::DefaultGatewayChanged {
                old_gateway: *old_gateway,
                new_gateway: *new_gateway,
                local_address: local_address.clone(),
            },
            address_mapper::Event::LocalAddressChanged(addr) => {
                Self::LocalAddressChanged(addr.clone())
            }
            address_mapper::Event::NewExternalMappedAddress {
                local_address,
                external_address,
            } => Self::NewExternalMappedAddress {
                local_address: local_address.clone(),
                external_address: external_address.clone(),
            },
        })
    }
}

impl TryFrom<FromSwarm<'_>> for Event {
    type Error = ();

    fn try_from(event: FromSwarm<'_>) -> Result<Self, Self::Error> {
        match event {
            FromSwarm::NewListenAddr(NewListenAddr { addr, .. }) => {
                Ok(Self::NewListenAddress(addr.clone()))
            }
            FromSwarm::ExternalAddrConfirmed(ExternalAddrConfirmed { addr }) => {
                Ok(Self::ExternalAddressConfirmed(addr.clone()))
            }
            _ => Err(()),
        }
    }
}
