use std::net::Ipv4Addr;

use color_eyre::eyre::{Result, bail, eyre};
use lb_core::sdp::{
    Locator, Locators, ServiceType,
    genesis::{ProviderInfo, StakeHolderInfo},
};
use lb_libp2p::{Multiaddr, Protocol};

use super::ParticipateArgs;
use crate::{
    UserConfig,
    config::{OnUnknownKeys, deserialize_config_at_path, network::serde::nat},
};

pub fn run(args: &ParticipateArgs) -> Result<()> {
    let user_config = deserialize_config_at_path::<UserConfig>(&args.config, OnUnknownKeys::Warn)?;

    let (_, zk_id) = user_config.blend_zk_key().map_err(|e| eyre!("{e}"))?;
    let provider_id = user_config
        .blend_provider_id()
        .map_err(|e| eyre!("{e}"))
        .map(|p| p.0)?;

    let listen_addr = &user_config.blend.core.backend.listening_address;
    let nat_config = &user_config.network.backend.swarm.nat;
    let locator_addr = resolve_locator_addr(listen_addr, nat_config, args.external_address)?;
    let locator = Locator::try_from(locator_addr).map_err(|e| eyre!("{e}"))?;

    let stakeholder = StakeHolderInfo {
        zk_id,
        stake: args.stake,
    };
    let provider = ProviderInfo {
        provider_id,
        zk_id,
        locators: Locators::from(locator),
        service_type: ServiceType::BlendNetwork,
    };

    std::fs::create_dir_all(&args.output)?;
    let stakeholder_path = args.output.join("stakeholder.yaml");
    let provider_path = args.output.join("provider.yaml");

    std::fs::write(&stakeholder_path, serde_yaml::to_string(&stakeholder)?)?;
    std::fs::write(&provider_path, serde_yaml::to_string(&provider)?)?;

    println!("Written: {}", stakeholder_path.display());
    println!("Written: {}", provider_path.display());

    Ok(())
}

/// Resolves the blend locator address, replacing an unspecified host with a
/// real one determined by (in order of priority):
/// 1. `--external-address` CLI flag
/// 2. The host protocol extracted from the network NAT static config
fn resolve_locator_addr(
    addr: &Multiaddr,
    nat: &nat::Config,
    external: Option<Ipv4Addr>,
) -> Result<Multiaddr> {
    if !has_unspecified_host(addr) {
        return Ok(addr.clone());
    }

    if let Some(ip) = external {
        return Ok(replace_host(addr, Protocol::Ip4(ip)));
    }

    if let Some(resolved) = replace_host_from_nat(addr, nat) {
        return Ok(resolved);
    }

    bail!(
        "Blend listening address is {addr} (unspecified host). \
         Set a static external address in the network NAT config or \
         provide --external-address with the node's public IPv4 address."
    );
}

/// Returns true if the first protocol in `addr` is an unspecified address
/// (0.0.0.0 for IPv4 or :: for IPv6).
fn has_unspecified_host(addr: &Multiaddr) -> bool {
    addr.iter().next().is_some_and(|p| match p {
        Protocol::Ip4(ip) => ip.is_unspecified(),
        Protocol::Ip6(ip) => ip.is_unspecified(),
        _ => false,
    })
}

/// Builds a new multiaddr by replacing the first (host) protocol of `addr`
/// with the host protocol taken from the NAT static external address.
/// Returns `None` if NAT is not configured as static.
fn replace_host_from_nat(addr: &Multiaddr, nat: &nat::Config) -> Option<Multiaddr> {
    let nat::Config::Static { external_address } = nat else {
        return None;
    };
    let new_host = external_address.iter().next()?;
    Some(replace_host(addr, new_host))
}

/// Returns a new multiaddr where the first (host) protocol is `new_host` and
/// the remaining protocols are taken unchanged from `addr`.
fn replace_host(addr: &Multiaddr, new_host: Protocol<'_>) -> Multiaddr {
    let mut result = Multiaddr::empty();
    result.push(new_host);
    for proto in addr.iter().skip(1) {
        result.push(proto);
    }
    result
}
