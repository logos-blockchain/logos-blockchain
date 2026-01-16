use multiaddr::Multiaddr;
use upnp::UpnpProtocol;

use crate::{
    NatMappingSettings,
    behaviour::nat::address_mapper::{
        errors::AddressMapperError,
        protocols::{nat_pmp::NatPmp, pcp::PcpProtocol},
    },
};

mod nat_pmp;
mod pcp;
mod pcp_core;
mod upnp;

#[async_trait::async_trait]
pub trait NatMapper: Send + Sync + 'static {
    /// Tries to map an address and returns the external address
    async fn map_address(
        address: &Multiaddr,
        settings: NatMappingSettings,
    ) -> Result<Multiaddr, AddressMapperError>;
}

pub struct ProtocolManager;

#[async_trait::async_trait]
impl NatMapper for ProtocolManager {
    async fn map_address(
        address: &Multiaddr,
        settings: NatMappingSettings,
    ) -> Result<Multiaddr, AddressMapperError> {
        if let Ok(external_address) = PcpProtocol::map_address(address, settings).await {
            tracing::info!("Successfully mapped {address} to {external_address} using PCP");
            return Ok(external_address);
        }

        if let Ok(external_address) = NatPmp::map_address(address, settings).await {
            tracing::info!("Successfully mapped {address} to {external_address} using NAT-PMP");

            return Ok(external_address);
        }

        let external_address = UpnpProtocol::map_address(address, settings).await?;
        tracing::info!("Successfully mapped {address} to {external_address} using UPnP");

        Ok(external_address)
    }
}

#[cfg(test)]
mod real_gateway_tests {
    use rand::{Rng as _, thread_rng};

    use super::*;

    /// Run with: `NAT_TEST_LOCAL_IP="192.168.1.100`" cargo test
    /// `map_address_via_protocol_manager_real_gateway` -- --ignored
    #[tokio::test]
    #[ignore = "Runs against a real gateway"]
    async fn map_address_via_protocol_manager_real_gateway() {
        let local_ip = std::env::var("NAT_TEST_LOCAL_IP").expect("NAT_TEST_LOCAL_IP");
        let random_port: u64 = thread_rng().gen_range(10000..=64000);
        let local_address = format!("/ip4/{local_ip}/tcp/{random_port}");

        println!("Testing NAT mapping for local address: {local_address}",);

        let local_address: Multiaddr = local_address.parse().expect("valid multiaddr");

        let settings = NatMappingSettings::default();
        let external = ProtocolManager::map_address(&local_address, settings)
            .await
            .expect("map address");

        println!("Successfully mapped {local_address} to {external}");
    }
}
