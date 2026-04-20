use core::str::FromStr as _;
use std::collections::HashMap;

use color_eyre::eyre::{Result, eyre};
use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Ed25519Key, Key, ZkKey, ZkPublicKey, secured_key::SecuredKey as _},
};
use libp2p::{Multiaddr, PeerId};
use num_bigint::BigUint;
use rand::rngs::OsRng;

use crate::{
    UserConfig,
    config::{
        ApiConfig, InitArgs, KmsConfig, SdpConfig, StateConfig, StorageConfig, TracingConfig,
        WalletConfig,
        blend::serde::{Config as BlendConfig, RequiredValues as BlendConfigRequiredValues},
        cryptarchia::serde::{
            Config as CryptarchiaConfig, RequiredValues as CryptarchiaConfigRequiredValues,
        },
        network::serde::{Config as NetworkConfig, nat},
        sdp::serde::RequiredValues as SdpRequiredValues,
        time::serde::Config as TimeConfig,
        wallet::serde::RequiredValues as WalletConfigRequiredValues,
    },
};

fn key_id(key: &Key) -> KeyId {
    let key_id_bytes = match key {
        Key::Ed25519(ed25519_secret_key) => ed25519_secret_key.as_public_key().to_bytes(),
        Key::Zk(zk_secret_key) => fr_to_bytes(zk_secret_key.as_public_key().as_fr()),
    };
    hex::encode(key_id_bytes)
}

fn generate_zk_key_from_random_bytes() -> ZkKey {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut OsRng, &mut bytes);
    ZkKey::from(BigUint::from_bytes_le(&bytes))
}

struct GeneratedKeys {
    blend_signing_key: Ed25519Key,
    blend_zk_key: ZkKey,
    leader_key: ZkKey,
    funding_key: ZkKey,
    blend_signing_key_id: KeyId,
    blend_zk_key_id: KeyId,
    leader_key_id: KeyId,
    funding_key_id: KeyId,
    leader_pk: ZkPublicKey,
    funding_pk: ZkPublicKey,
}

fn generate_keys() -> GeneratedKeys {
    let blend_signing_key = Ed25519Key::generate(&mut OsRng);
    let blend_zk_key = ZkKey::from(BigUint::from_bytes_le(
        blend_signing_key.public_key().as_bytes(),
    ));
    let leader_key = generate_zk_key_from_random_bytes();
    let funding_key = generate_zk_key_from_random_bytes();

    let blend_signing_key_id = key_id(&blend_signing_key.clone().into());
    let blend_zk_key_id = key_id(&blend_zk_key.clone().into());
    let leader_key_id = key_id(&leader_key.clone().into());
    let funding_key_id = key_id(&funding_key.clone().into());

    let leader_pk: ZkPublicKey = leader_key.as_public_key();
    let funding_pk: ZkPublicKey = funding_key.as_public_key();

    GeneratedKeys {
        blend_signing_key,
        blend_zk_key,
        leader_key,
        funding_key,
        blend_signing_key_id,
        blend_zk_key_id,
        leader_key_id,
        funding_key_id,
        leader_pk,
        funding_pk,
    }
}

pub fn run(args: &InitArgs) -> Result<()> {
    if args.initial_peers.is_empty() {
        eprintln!("Warning: No initial peers provided. This node will start as a genesis node.");
    }

    let network_key = lb_libp2p::ed25519::SecretKey::generate();
    let keys = generate_keys();

    let blend_listening_address =
        Multiaddr::from_str(&format!("/ip4/0.0.0.0/udp/{}/quic-v1", args.blend_port))
            .map_err(|e| eyre!("Invalid blend listening address: {e}"))?;

    let user_config = build_user_config(args, network_key, keys, blend_listening_address);

    let yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(&args.output, &yaml)?;

    println!("Config written to {}", args.output.display());
    Ok(())
}

fn build_user_config(
    args: &InitArgs,
    network_key: lb_libp2p::ed25519::SecretKey,
    keys: GeneratedKeys,
    blend_listening_address: Multiaddr,
) -> UserConfig {
    let GeneratedKeys {
        blend_signing_key,
        blend_zk_key,
        leader_key,
        funding_key,
        blend_signing_key_id,
        blend_zk_key_id,
        leader_key_id,
        funding_key_id,
        leader_pk,
        funding_pk,
    } = keys;

    let state_config = args
        .state_path
        .as_ref()
        .map_or_else(StateConfig::default, |path| StateConfig {
            base_folder: path.clone(),
        });

    let network_config = {
        let mut base_config = NetworkConfig::default();
        base_config.backend.swarm.port = args.net_port;
        base_config.backend.swarm.node_key = network_key;
        base_config
            .backend
            .initial_peers
            .clone_from(&args.initial_peers);
        if let Some(external_address) = &args.external_address {
            base_config.backend.swarm.nat = nat::Config::Static {
                external_address: external_address.clone(),
            };
        }

        base_config
    };

    let blend_config = {
        let mut base_config = BlendConfig::with_required_values(BlendConfigRequiredValues {
            non_ephemeral_signing_key_id: blend_signing_key_id.clone(),
            secret_key_kms_id: blend_zk_key_id.clone(),
        });
        base_config.set_listening_address(blend_listening_address);
        base_config
    };

    let cryptarchia_config = {
        let mut base_config =
            CryptarchiaConfig::with_required_values(CryptarchiaConfigRequiredValues { funding_pk });
        base_config.network.bootstrap.ibd.peers = args
            .initial_peers
            .iter()
            .filter_map(|addr| match addr.iter().last() {
                Some(lb_libp2p::Protocol::P2p(bytes)) => PeerId::from_multihash(bytes.into()).ok(),
                _ => None,
            })
            .collect();
        base_config
    };

    let time_config = TimeConfig::default();

    let tracing_config = TracingConfig::default();

    let sdp_config = SdpConfig::with_required_values(SdpRequiredValues { funding_pk });

    let api_config = {
        let mut base_config = ApiConfig::default();
        base_config.backend.listen_address = args.http_addr;
        base_config
    };

    let storage_config = StorageConfig::default();

    let kms_config = {
        let mut base_config = KmsConfig::default();
        base_config.backend.keys = HashMap::from([
            (blend_signing_key_id, blend_signing_key.into()),
            (blend_zk_key_id, blend_zk_key.into()),
            (leader_key_id.clone(), leader_key.into()),
            (funding_key_id.clone(), funding_key.into()),
        ]);
        base_config
    };

    let wallet_config = {
        let mut base_config = WalletConfig::with_required_values(WalletConfigRequiredValues {
            voucher_master_key_id: leader_key_id.clone(),
        });
        base_config.known_keys = [(leader_key_id, leader_pk), (funding_key_id, funding_pk)]
            .into_iter()
            .collect();
        base_config
    };

    UserConfig {
        network: network_config,
        blend: blend_config,
        cryptarchia: cryptarchia_config,
        time: time_config,
        tracing: tracing_config,
        sdp: sdp_config,
        api: api_config,
        storage: storage_config,
        kms: kms_config,
        wallet: wallet_config,
        state: state_config,
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;

    fn build_config_from_peers(initial_peers: Vec<Multiaddr>) -> UserConfig {
        let args = InitArgs {
            initial_peers,
            output: "test_output.yaml".into(),
            net_port: 3000,
            blend_port: 3400,
            http_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            external_address: None,
            state_path: None,
        };
        let network_key = lb_libp2p::ed25519::SecretKey::generate();
        let keys = generate_keys();
        let blend_addr = Multiaddr::from_str("/ip4/0.0.0.0/udp/3400/quic-v1").unwrap();
        build_user_config(&args, network_key, keys, blend_addr)
    }

    #[test]
    fn extracts_peer_ids_into_ibd_config() {
        let peer1 = PeerId::random();
        let peer2 = PeerId::random();

        let addr_with_p2p_1: Multiaddr = format!("/ip4/1.2.3.4/udp/3000/quic-v1/p2p/{peer1}")
            .parse()
            .unwrap();
        let addr_with_p2p_2: Multiaddr = format!("/ip4/5.6.7.8/udp/3000/quic-v1/p2p/{peer2}")
            .parse()
            .unwrap();
        let addr_without_p2p: Multiaddr = "/ip4/9.10.11.12/udp/3000/quic-v1".parse().unwrap();

        let config =
            build_config_from_peers(vec![addr_with_p2p_1, addr_without_p2p, addr_with_p2p_2]);

        let ibd_peers = &config.cryptarchia.network.bootstrap.ibd.peers;
        assert_eq!(ibd_peers.len(), 2);
        assert!(ibd_peers.contains(&peer1));
        assert!(ibd_peers.contains(&peer2));
    }

    #[test]
    fn no_peer_ids_yields_empty_ibd_config() {
        let addr: Multiaddr = "/ip4/1.2.3.4/udp/3000/quic-v1".parse().unwrap();

        let config = build_config_from_peers(vec![addr]);

        assert!(config.cryptarchia.network.bootstrap.ibd.peers.is_empty());
    }
}
