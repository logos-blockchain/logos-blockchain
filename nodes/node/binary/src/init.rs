use core::str::FromStr as _;
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    num::NonZeroU64,
    time::Duration,
};

use color_eyre::eyre::{Result, eyre};
use lb_core::mantle::Value;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Ed25519Key, Key, ZkKey, ZkPublicKey, secured_key::SecuredKey as _},
};
use libp2p::Multiaddr;
use num_bigint::BigUint;
use rand::rngs::OsRng;

use crate::{
    UserConfig,
    config::{
        ApiConfig, InitArgs, KmsConfig, SdpConfig, StorageConfig, TracingConfig, WalletConfig,
        api::serde::AxumBackendSettings,
        blend::serde::{
            Config as BlendConfig,
            core::{
                BackendConfig as BlendCoreBackendConfig, Config as BlendCoreConfig, ZkSettings,
            },
            edge::{BackendConfig as BlendEdgeBackendConfig, Config as BlendEdgeConfig},
        },
        cryptarchia::serde::{
            Config as CryptarchiaConfig,
            leader::{
                Config as CryptarchiaLeaderConfig, WalletConfig as CryptarchiaLeaderWalletConfig,
            },
            network::{
                BootstrapConfig as CryptarchiaNetworkBootstrapConfig,
                Config as CryptarchiaNetworkConfig, IbdConfig, OrphanConfig, SyncConfig,
            },
            service::{
                BootstrapConfig as CryptarchiaBootstrapConfig, Config as CryptarchiaServiceConfig,
                OfflineGracePeriodConfig,
            },
        },
        kms::serde::PreloadKmsBackendSettings,
        mempool::serde::Config as MempoolConfig,
        network::serde::{self as network, BackendSettings, Config as NetworkConfig, SwarmConfig},
        sdp::serde::WalletConfig as SdpWalletConfig,
        storage::serde::RocksDbSettings,
        time::serde::{Config as TimeConfig, NtpClientSettings, NtpSettings},
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

#[expect(
    clippy::too_many_lines,
    reason = "Single struct literal assembling all config fields."
)]
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

    UserConfig {
        network: NetworkConfig {
            backend: BackendSettings {
                swarm: SwarmConfig {
                    host: Ipv4Addr::UNSPECIFIED,
                    port: args.net_port,
                    node_key: network_key,
                    gossipsub: lb_libp2p::gossipsub::Config::default(),
                    kademlia: network::kademlia::Config::default(),
                    identify: network::identify::Config::default(),
                    chain_sync: network::chainsync::Config::default(),
                    nat: args.external_address.as_ref().map_or_else(
                        network::nat::Config::default,
                        |addr| network::nat::Config::Static {
                            external_address: addr.clone(),
                        },
                    ),
                },
                initial_peers: args.initial_peers.clone(),
            },
        },
        blend: BlendConfig {
            non_ephemeral_signing_key_id: blend_signing_key_id.clone(),
            recovery_path_prefix: "./recovery/blend".into(),
            core: BlendCoreConfig {
                backend: BlendCoreBackendConfig {
                    listening_address: blend_listening_address,
                    core_peering_degree: 1..=3,
                    edge_node_connection_timeout: Duration::from_secs(5),
                    max_edge_node_incoming_connections: 300,
                    max_dial_attempts_per_peer: NonZeroU64::new(3)
                        .expect("Max dial attempts per peer cannot be zero."),
                },
                zk: ZkSettings {
                    secret_key_kms_id: blend_zk_key_id.clone(),
                },
            },
            edge: BlendEdgeConfig {
                backend: BlendEdgeBackendConfig {
                    max_dial_attempts_per_peer_per_message: NonZeroU64::new(3)
                        .expect("cannot be zero"),
                    replication_factor: NonZeroU64::new(1).expect("cannot be zero"),
                },
            },
        },
        cryptarchia: CryptarchiaConfig {
            service: CryptarchiaServiceConfig {
                recovery_file: "./recovery/cryptarchia.json".into(),
                bootstrap: CryptarchiaBootstrapConfig {
                    prolonged_bootstrap_period: Duration::from_secs(60),
                    force_bootstrap: false,
                    offline_grace_period: OfflineGracePeriodConfig::default(),
                },
            },
            network: CryptarchiaNetworkConfig {
                bootstrap: CryptarchiaNetworkBootstrapConfig {
                    ibd: IbdConfig {
                        peers: HashSet::new(),
                        delay_before_new_download: Duration::from_secs(10),
                    },
                },
                sync: SyncConfig {
                    orphan: OrphanConfig {
                        max_orphan_cache_size: std::num::NonZeroUsize::new(5)
                            .expect("Max orphan cache size must be non-zero"),
                    },
                },
            },
            leader: CryptarchiaLeaderConfig {
                wallet: CryptarchiaLeaderWalletConfig {
                    max_tx_fee: Value::MAX,
                    funding_pk,
                },
            },
        },
        time: TimeConfig {
            backend: NtpSettings {
                server: "pool.ntp.org:123".to_owned(),
                client: NtpClientSettings {
                    timeout: Duration::from_secs(5),
                    listening_interface: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                },
                update_interval: Duration::from_secs(16),
            },
        },
        mempool: MempoolConfig {
            recovery_path: "./recovery/mempool.json".into(),
        },
        tracing: TracingConfig::default(),
        sdp: SdpConfig {
            declaration: None,
            wallet: SdpWalletConfig {
                max_tx_fee: Value::MAX,
                funding_pk,
            },
        },
        api: ApiConfig {
            backend: AxumBackendSettings {
                address: args.http_addr,
                ..AxumBackendSettings::default()
            },
            #[cfg(feature = "testing")]
            testing: AxumBackendSettings::default(),
        },
        storage: StorageConfig {
            backend: RocksDbSettings {
                path: "./db".into(),
                read_only: false,
                column_family: Some("blocks".into()),
            },
        },
        kms: KmsConfig {
            backend: PreloadKmsBackendSettings {
                keys: HashMap::from([
                    (blend_signing_key_id, blend_signing_key.into()),
                    (blend_zk_key_id, blend_zk_key.into()),
                    (leader_key_id.clone(), leader_key.into()),
                    (funding_key_id.clone(), funding_key.into()),
                ]),
            },
        },
        wallet: WalletConfig {
            known_keys: HashMap::from([
                (leader_key_id.clone(), leader_pk),
                (funding_key_id, funding_pk),
            ]),
            voucher_master_key_id: leader_key_id,
            recovery_path: "./recovery/wallet.json".into(),
        },
    }
}
