use std::{net::Ipv4Addr, num::NonZeroU16};

use color_eyre::eyre::{Result, bail, eyre};
use lb_core::sdp::{DeclarationId, Locator, Locators, ProviderId, ServiceType};
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey, kdf_public_key};
use lb_libp2p::{Multiaddr, Protocol};
use lb_sdp_service::DeclarationRequest;
use serde::Serialize;

use super::ParticipateArgs;
use crate::{
    UserConfig,
    config::{
        keystore::{KeyTitle, Keystore},
        network::serde::nat,
    },
};

#[derive(Serialize)]
struct ParticipationData {
    stakeholder_identities: Vec<ZkPublicKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blend: Option<BlendParticipationData>,
}

#[derive(Serialize)]
struct BlendParticipationData {
    provider_id: Ed25519PublicKey,
    zk_id: ZkPublicKey,
    note_pk: ZkPublicKey,
    note_stake_pk: ZkPublicKey,
    locators: Locators,
    service_type: ServiceType,
    declaration_id: DeclarationId,
}

pub fn run(args: &ParticipateArgs) -> Result<()> {
    let user_config = UserConfig::load(&args.config)?;
    let keystore = Keystore::load(&user_config.kms.keystore_path(&args.config))?;

    let (_, stake_key) = keystore.get_zk(KeyTitle::STAKE)?;
    let (_, leader_funding_key) = keystore.get_zk(KeyTitle::LEADER_FUNDING)?;
    let (_, sdp_funding_key) = keystore.get_zk(KeyTitle::SDP_FUNDING)?;

    let blend = build_blend_data(
        &user_config,
        &keystore,
        args.external_address,
        args.external_port,
    )?;
    let data = ParticipationData {
        stakeholder_identities: stakeholder_identities(
            [
                stake_key.to_public_key(),
                leader_funding_key.to_public_key(),
                sdp_funding_key.to_public_key(),
            ],
            blend.as_ref(),
        ),
        blend,
    };

    std::fs::create_dir_all(&args.output)?;
    let output_path = args.output.join("participation_data.yaml");
    std::fs::write(&output_path, serde_yaml::to_string(&data)?)?;
    println!("Written: {}", output_path.display());

    Ok(())
}

/// The keys the genesis allocates notes to: the note keys of the wallet, and
/// the one of the Blend key when it joins. A stakeholder identity is the public
/// key of a note, so the Blend key gives its note public key, which its wallet
/// spends, not its quota identity `zk_id`, which stays the provider's.
fn stakeholder_identities(
    wallet_keys: impl IntoIterator<Item = ZkPublicKey>,
    blend: Option<&BlendParticipationData>,
) -> Vec<ZkPublicKey> {
    wallet_keys
        .into_iter()
        .chain(blend.map(|blend| blend.note_pk))
        .collect()
}

/// Returns `Ok(Some(...))` when the blend signing key is present and the
/// locator can be resolved, `Ok(None)` when the signing key is absent, or
/// `Err` when the key is present but address resolution fails.
fn build_blend_data(
    user_config: &UserConfig,
    keystore: &Keystore,
    external_address: Option<Ipv4Addr>,
    external_port: Option<NonZeroU16>,
) -> Result<Option<BlendParticipationData>> {
    let provider_id = match user_config.blend_provider_id() {
        Ok(id) => id.0,
        Err(_) => return Ok(None),
    };

    let listen_addr = &user_config.blend.core.backend.listening_address;
    let nat_config = &user_config.network.backend.swarm.nat;
    let locator_addr = resolve_locator_addr(listen_addr, nat_config, external_address)
        .and_then(|resolved_ip_addr| resolve_locator_port(resolved_ip_addr, external_port))?;
    let locators = Locators::from(Locator::try_from(locator_addr).map_err(|e| eyre!("{e}"))?);
    let (_, blend_key) = keystore.get_zk(KeyTitle::BLEND_ZK)?;
    // The zk_id is derived from the quota secret key itself, which the Proof of
    // Quota proves, and the service note is under the public key of the notes
    // of the same key, which the wallet spends.
    let zk_id = kdf_public_key(blend_key.as_fr());

    // Declaration ID is not required for the genesis ceremony, but it is still
    // useful for looking the declaration up once it is on chain. The stake is
    // not part of the ID, so a zero one is fine here.
    let declaration_id = DeclarationRequest {
        service_type: ServiceType::BlendNetwork,
        locators: locators.clone(),
        provider_id: ProviderId(provider_id),
        zk_id,
        note_pk: blend_key.to_public_key(),
        note_stake_pk: blend_key.to_public_key(),
        amount: 0,
    }
    .id();

    Ok(Some(BlendParticipationData {
        provider_id,
        zk_id,
        note_pk: blend_key.to_public_key(),
        note_stake_pk: blend_key.to_public_key(),
        locators,
        service_type: ServiceType::BlendNetwork,
        declaration_id,
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

/// Resolves the blend locator port, replacing port 0 (the OS picks the
/// listening port when the node starts) with the `--external-port` CLI flag.
fn resolve_locator_port(addr: Multiaddr, external: Option<NonZeroU16>) -> Result<Multiaddr> {
    if !addr.iter().any(|p| matches!(p, Protocol::Udp(0))) {
        return Ok(addr);
    }

    let Some(port) = external else {
        bail!(
            "Blend listening port is 0 (assigned by the OS when the node starts). \
             Provide --external-port with the node's public blend port."
        );
    };

    Ok(addr
        .iter()
        .map(|p| match p {
            Protocol::Udp(0) => Protocol::Udp(port.get()),
            p => p,
        })
        .collect())
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

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::ZkKey;

    use super::*;

    /// A Blend key joining the genesis is allocated its note under the public
    /// key its wallet spends, while the provider keeps the quota identity.
    #[test]
    fn the_blend_stakeholder_identity_is_its_note_public_key() {
        let blend_key = ZkKey::new(Fr::from(7u64));
        let note_pk = blend_key.to_public_key();
        let zk_id = kdf_public_key(blend_key.as_fr());
        assert_ne!(note_pk, zk_id, "the two identities differ");
        let blend = BlendParticipationData {
            provider_id: Ed25519PublicKey::from_bytes(&[1; 32]).unwrap(),
            zk_id,
            note_pk,
            note_stake_pk: note_pk,
            locators: Locators::from(
                Locator::try_from("/ip4/1.2.3.4/udp/3000".parse::<Multiaddr>().unwrap()).unwrap(),
            ),
            service_type: ServiceType::BlendNetwork,
            declaration_id: DeclarationId([0; 32]),
        };
        let wallet = ZkPublicKey::new(Fr::from(1u64));

        assert_eq!(
            stakeholder_identities([wallet], Some(&blend)),
            vec![wallet, note_pk]
        );
        assert_eq!(stakeholder_identities([wallet], None), vec![wallet]);
    }
}
