use std::net::Ipv4Addr;

use color_eyre::eyre::{Result, bail, eyre};
use lb_core::sdp::{Locator, Locators, ServiceType};
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};
use lb_libp2p::{Multiaddr, Protocol};
use serde::Serialize;

use super::ParticipateArgs;
use crate::{
    UserConfig,
    config::{OnUnknownKeys, deserialize_config_at_path, network::serde::nat},
};

#[derive(Serialize)]
struct ParticipationData {
    stakeholder_identity: ZkPublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    blend: Option<BlendParticipationData>,
}

#[derive(Serialize)]
struct BlendParticipationData {
    provider_id: Ed25519PublicKey,
    locators: Locators,
    service_type: ServiceType,
}

pub fn run(args: &ParticipateArgs) -> Result<()> {
    let user_config = deserialize_config_at_path::<UserConfig>(&args.config, OnUnknownKeys::Warn)?;

    let (_, stakeholder_identity) = user_config.blend_zk_key().map_err(|e| eyre!("{e}"))?;
    let data = ParticipationData {
        stakeholder_identity,
        blend: build_blend_data(&user_config, args.external_address)?,
    };

    std::fs::create_dir_all(&args.output)?;
    let output_path = args.output.join("participation_data.yaml");
    std::fs::write(&output_path, serde_yaml::to_string(&data)?)?;
    println!("Written: {}", output_path.display());

    Ok(())
}

/// Returns `Ok(Some(...))` when the blend signing key is present and the
/// locator can be resolved, `Ok(None)` when the signing key is absent, or
/// `Err` when the key is present but address resolution fails.
fn build_blend_data(
    user_config: &UserConfig,
    external_address: Option<Ipv4Addr>,
) -> Result<Option<BlendParticipationData>> {
    let provider_id = match user_config.blend_provider_id() {
        Ok(id) => id.0,
        Err(_) => return Ok(None),
    };

    let listen_addr = &user_config.blend.core.backend.listening_address;
    let nat_config = &user_config.network.backend.swarm.nat;
    let locator_addr = resolve_locator_addr(listen_addr, nat_config, external_address)?;
    let locator = Locator::try_from(locator_addr).map_err(|e| eyre!("{e}"))?;

    Ok(Some(BlendParticipationData {
        provider_id,
        locators: Locators::from(locator),
        service_type: ServiceType::BlendNetwork,
    }))
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
