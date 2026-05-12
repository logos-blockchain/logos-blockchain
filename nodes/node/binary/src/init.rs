use core::str::FromStr as _;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

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
        update_tracing_filter_and_derive_level,
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
    blend_zk_pk: ZkPublicKey,
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
    let blend_zk_pk: ZkPublicKey = blend_zk_key.as_public_key();

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
        blend_zk_pk,
    }
}

pub fn run(args: &InitArgs) -> Result<()> {
    if args.initial_peers.is_empty() {
        eprintln!("Warning: No initial peers provided. This node will start as a genesis node.");
    }

    let network_key = lb_libp2p::ed25519::SecretKey::generate();

    let blend_listening_address =
        Multiaddr::from_str(&format!("/ip4/0.0.0.0/udp/{}/quic-v1", args.blend_port))
            .map_err(|e| eyre!("Invalid blend listening address: {e}"))?;

    // If --kms-file points to an existing file, reuse its keys instead of
    // generating new ones.
    let (keys, kms_path, write_kms) = match &args.kms_file {
        Some(path) if path.exists() => {
            let contents = std::fs::read_to_string(path)?;
            let kms_config: KmsConfig = serde_yaml::from_str(&contents)?;
            let keys = keys_from_kms_config(&kms_config)?;
            (keys, path.clone(), false)
        }
        _ => {
            let keys = generate_keys();
            let path = kms_output_path(args);
            (keys, path, true)
        }
    };

    let tracing_config = build_tracing_config(args)?;
    let user_config = build_user_config(
        args,
        network_key,
        keys,
        blend_listening_address,
        tracing_config,
    );

    if write_kms {
        let kms_yaml = serde_yaml::to_string(&user_config.kms)?;
        std::fs::write(&kms_path, &kms_yaml)?;
        println!("KMS config written to {}", kms_path.display());
    }

    let kms_include = kms_include_str(args, &kms_path);
    let yaml = serialize_with_kms_include(&user_config, &kms_include)?;
    std::fs::write(&args.output, &yaml)?;

    println!("Config written to {}", args.output.display());
    Ok(())
}

/// Extracts key material from a parsed KMS config so it can be used to
/// populate the rest of the node config without generating new keys.
///
/// Assumes the KMS config was produced by `init` and follows the layout:
/// - exactly one Ed25519 key  → blend signing key
/// - one Zk key derived from the Ed25519 public bytes → blend Zk key
/// - remaining Zk keys sorted by ID: first → leader, second → funding
fn keys_from_kms_config(kms_config: &KmsConfig) -> Result<GeneratedKeys> {
    let (blend_signing_key_id, blend_signing_key) = kms_config
        .backend
        .keys
        .iter()
        .find_map(|(id, key)| match key {
            Key::Ed25519(k) => Some((id.clone(), k.clone())),
            Key::Zk(_) => None,
        })
        .ok_or_else(|| eyre!("KMS file contains no Ed25519 key"))?;

    let blend_zk_key = ZkKey::from(BigUint::from_bytes_le(
        blend_signing_key.public_key().as_bytes(),
    ));
    let blend_zk_key_id = key_id(&Key::Zk(blend_zk_key.clone()));

    let mut other_zk: Vec<(KeyId, ZkKey)> = kms_config
        .backend
        .keys
        .iter()
        .filter_map(|(id, key)| match key {
            Key::Zk(k) if *id != blend_zk_key_id => Some((id.clone(), k.clone())),
            _ => None,
        })
        .collect();

    other_zk.sort_by(|(a, _), (b, _)| a.cmp(b));

    if other_zk.len() < 2 {
        return Err(eyre!(
            "KMS file must contain at least 3 Zk keys (blend, leader, funding), found {}",
            other_zk.len() + 1
        ));
    }

    let (leader_key_id, leader_key) = other_zk.remove(0);
    let (funding_key_id, funding_key) = other_zk.remove(0);
    let leader_pk = leader_key.as_public_key();
    let funding_pk = funding_key.as_public_key();
    let blend_zk_pk = blend_zk_key.as_public_key();

    Ok(GeneratedKeys {
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
        blend_zk_pk,
    })
}

fn kms_output_path(args: &InitArgs) -> PathBuf {
    args.kms_file.clone().unwrap_or_else(|| {
        args.output
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("kms.yaml")
    })
}

/// Returns the path string to use in `!include <path>` inside the main config.
///
/// If the KMS file is in the same directory as the main config, only the
/// filename is used so the include stays portable when the directory is moved.
fn kms_include_str(args: &InitArgs, kms_path: &Path) -> String {
    if let Some(ref kms_file) = args.kms_file {
        return kms_file.to_string_lossy().into_owned();
    }
    // Default: same directory as the output file → reference by filename only.
    kms_path.file_name().map_or_else(
        || kms_path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn serialize_with_kms_include(config: &UserConfig, kms_include: &str) -> Result<String> {
    use serde_yaml::value::{Tag, TaggedValue};
    let mut value = serde_yaml::to_value(config)?;
    if let serde_yaml::Value::Mapping(ref mut map) = value {
        map.insert(
            serde_yaml::Value::String("kms".into()),
            serde_yaml::Value::Tagged(Box::new(TaggedValue {
                tag: Tag::new("include"),
                value: serde_yaml::Value::String(kms_include.into()),
            })),
        );
    }
    Ok(serde_yaml::to_string(&value)?)
}

fn build_tracing_config(args: &InitArgs) -> Result<TracingConfig> {
    let mut tracing_config = TracingConfig::default();

    if let Some(filter) = &args.log_filter {
        update_tracing_filter_and_derive_level(&mut tracing_config, filter)?;
    }

    Ok(tracing_config)
}

fn build_user_config(
    args: &InitArgs,
    network_key: lb_libp2p::ed25519::SecretKey,
    keys: GeneratedKeys,
    blend_listening_address: Multiaddr,
    tracing_config: TracingConfig,
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
        blend_zk_pk,
    } = keys;

    let state_config = args
        .state_path
        .as_ref()
        .map_or_else(StateConfig::default, |path| StateConfig {
            base_folder: path.clone(),
        });

    let network_config = build_network_config(args, network_key);

    let blend_config = {
        let mut base_config = BlendConfig::with_required_values(BlendConfigRequiredValues {
            non_ephemeral_signing_key_id: blend_signing_key_id.clone(),
            secret_key_kms_id: blend_zk_key_id.clone(),
        });
        base_config.set_listening_address(blend_listening_address);
        base_config
    };

    let cryptarchia_config = build_cryptarchia_config(args, funding_pk);

    let time_config = TimeConfig::default();

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
            (blend_zk_key_id.clone(), blend_zk_key.into()),
            (leader_key_id.clone(), leader_key.into()),
            (funding_key_id.clone(), funding_key.into()),
        ]);
        base_config
    };

    let wallet_config = {
        let mut base_config = WalletConfig::with_required_values(WalletConfigRequiredValues {
            voucher_master_key_id: leader_key_id.clone(),
        });
        base_config.known_keys = [
            (leader_key_id, leader_pk),
            (funding_key_id, funding_pk),
            (blend_zk_key_id, blend_zk_pk),
        ]
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

fn build_network_config(args: &InitArgs, node_key: lb_libp2p::ed25519::SecretKey) -> NetworkConfig {
    let mut base_config = NetworkConfig::default();
    base_config.backend.swarm.port = args.net_port;
    base_config.backend.swarm.node_key = node_key;
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
}

fn build_cryptarchia_config(args: &InitArgs, funding_pk: ZkPublicKey) -> CryptarchiaConfig {
    let mut base_config =
        CryptarchiaConfig::with_required_values(CryptarchiaConfigRequiredValues { funding_pk });
    base_config.network.bootstrap.ibd.peers = if args.ibd {
        args.initial_peers
            .iter()
            .filter_map(|addr| match addr.iter().last() {
                Some(lb_libp2p::Protocol::P2p(bytes)) => PeerId::from_multihash(bytes.into()).ok(),
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };
    base_config
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use crate::config::tracing::serde::{
        Level,
        filter::{EnvConfig, Layer},
    };

    fn build_config_from_peers(initial_peers: Vec<Multiaddr>) -> UserConfig {
        build_config(initial_peers, true)
    }

    fn build_config(initial_peers: Vec<Multiaddr>, ibd: bool) -> UserConfig {
        let args = InitArgs {
            initial_peers,
            output: "test_output.yaml".into(),
            net_port: 3000,
            blend_port: 3400,
            http_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            external_address: None,
            state_path: None,
            ibd,
            log_filter: None,
            kms_file: None,
        };
        let network_key = lb_libp2p::ed25519::SecretKey::generate();
        let keys = generate_keys();
        let blend_addr = Multiaddr::from_str("/ip4/0.0.0.0/udp/3400/quic-v1").unwrap();
        let tracing_config = build_tracing_config(&args).unwrap();

        build_user_config(&args, network_key, keys, blend_addr, tracing_config)
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

    #[test]
    fn no_ibd_flag_clears_ibd_peers_even_with_initial_peers() {
        let peer = PeerId::random();
        let addr_with_p2p: Multiaddr = format!("/ip4/1.2.3.4/udp/3000/quic-v1/p2p/{peer}")
            .parse()
            .unwrap();

        let config = build_config(vec![addr_with_p2p], false);

        assert!(config.cryptarchia.network.bootstrap.ibd.peers.is_empty());
    }

    #[test]
    fn log_flags_write_env_filter_config() {
        let args = InitArgs {
            initial_peers: Vec::new(),
            output: "test_output.yaml".into(),
            net_port: 3000,
            blend_port: 3400,
            http_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            external_address: None,
            state_path: None,
            ibd: false,
            log_filter: Some(
                "warn,logos_blockchain=debug,libp2p_gossipsub::behaviour=error".to_owned(),
            ),
            kms_file: None,
        };

        let tracing_config = build_tracing_config(&args).unwrap();
        let Layer::Env(EnvConfig { filters }) = tracing_config.filter else {
            panic!("expected env filter config");
        };

        assert_eq!(tracing_config.level, Level::DEBUG);
        assert_eq!(filters.get("*"), Some(&Level::WARN));
        assert_eq!(filters.get("logos_blockchain"), Some(&Level::DEBUG));
        assert_eq!(
            filters.get("libp2p_gossipsub::behaviour"),
            Some(&Level::ERROR)
        );
    }
}
