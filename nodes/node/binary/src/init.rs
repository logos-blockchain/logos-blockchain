use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    num::NonZeroU64,
    str::FromStr as _,
    time::Duration,
};

use color_eyre::eyre::{Result, eyre};
use lb_api_service::ApiServiceSettings;
use lb_blend_service::core::settings::ZkSettings;
use lb_chain_leader_service::LeaderWalletConfig;
use lb_chain_network_service::{IbdConfig, OrphanConfig, SyncConfig};
use lb_chain_service::OfflineGracePeriodConfig;
use lb_core::mantle::Value;
use lb_groth16::fr_to_bytes;
use lb_http_api_common::settings::AxumBackendSettings;
use lb_key_management_system_service::{
    backend::preload::{KeyId, PreloadKMSBackendSettings},
    keys::{Ed25519Key, Key, ZkKey, ZkPublicKey, secured_key::SecuredKey as _},
};
use lb_libp2p::Multiaddr;
use lb_sdp_service::{SdpSettings, wallet::SdpWalletConfig};
use lb_storage_service::backends::rocksdb::RocksBackendSettings;
use lb_time_service::backends::{NtpTimeBackendSettings, ntp::async_client::NTPClientSettings};
use lb_tracing_service::TracingSettings;
use lb_wallet_service::WalletServiceSettings;
use num_bigint::BigUint;
use rand::rngs::OsRng;

use crate::{
    UserConfig,
    config::{
        InitArgs,
        blend::serde::{
            Config as BlendConfig,
            core::{BackendConfig as BlendCoreBackendConfig, Config as BlendCoreConfig},
            edge::{BackendConfig as BlendEdgeBackendConfig, Config as BlendEdgeConfig},
        },
        cryptarchia::serde::{
            Config as CryptarchiaConfig, LeaderConfig, NetworkConfig as CryptarchiaNetworkConfig,
            ServiceConfig as CryptarchiaServiceConfig,
        },
        mempool::serde::Config as MempoolConfig,
        network::serde::{BackendSettings, Config as NetworkConfig, SwarmConfig},
        time::serde::Config as TimeConfig,
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

pub fn run(args: InitArgs) -> Result<()> {
    // 1. Generate Ed25519 network key (libp2p)
    let network_key = lb_libp2p::ed25519::SecretKey::generate();

    // 2. Generate Ed25519 blend signing key
    let blend_signing_key = Ed25519Key::generate(&mut OsRng);

    // 3. Derive ZK blend key from blend Ed25519 public key
    let blend_zk_key = ZkKey::from(BigUint::from_bytes_le(
        blend_signing_key.public_key().as_bytes(),
    ));

    // 4. Generate ZK leader/voucher key (random)
    let leader_key = generate_zk_key_from_random_bytes();

    // 5. Generate ZK SDP funding key (random)
    let funding_key = generate_zk_key_from_random_bytes();

    // Compute key IDs
    let blend_signing_key_id = key_id(&blend_signing_key.clone().into());
    let blend_zk_key_id = key_id(&blend_zk_key.clone().into());
    let leader_key_id = key_id(&leader_key.clone().into());
    let funding_key_id = key_id(&funding_key.clone().into());

    // Get public keys for wallet config
    let leader_pk: ZkPublicKey = leader_key.as_public_key();
    let funding_pk: ZkPublicKey = funding_key.as_public_key();

    let blend_listening_address = Multiaddr::from_str(&format!(
        "/ip4/0.0.0.0/udp/{}/quic-v1",
        args.blend_port
    ))
    .map_err(|e| eyre!("Invalid blend listening address: {e}"))?;

    let user_config = UserConfig {
        network: NetworkConfig {
            backend: BackendSettings {
                swarm: SwarmConfig {
                    host: Ipv4Addr::UNSPECIFIED,
                    port: args.net_port,
                    node_key: network_key,
                    gossipsub_config: lb_libp2p::gossipsub::Config::default(),
                    kademlia_config: Default::default(),
                    identify_config: Default::default(),
                    chain_sync_config: Default::default(),
                    nat_config: Default::default(),
                },
                initial_peers: args.initial_peers,
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
                bootstrap: lb_chain_service::BootstrapConfig {
                    prolonged_bootstrap_period: Duration::from_secs(60),
                    force_bootstrap: false,
                    offline_grace_period: OfflineGracePeriodConfig::default(),
                },
            },
            network: CryptarchiaNetworkConfig {
                bootstrap: lb_chain_network_service::BootstrapConfig {
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
            leader: LeaderConfig {
                wallet: LeaderWalletConfig {
                    max_tx_fee: Value::MAX,
                    funding_pk,
                },
            },
        },
        time: TimeConfig {
            backend: NtpTimeBackendSettings {
                ntp_server: "pool.ntp.org:123".to_owned(),
                ntp_client_settings: NTPClientSettings {
                    timeout: Duration::from_secs(5),
                    listening_interface: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                },
                update_interval: Duration::from_secs(16),
            },
        },
        mempool: MempoolConfig {
            recovery_path: "./recovery/mempool.json".into(),
        },
        tracing: TracingSettings::default(),
        sdp: SdpSettings {
            declaration: None,
            wallet_config: SdpWalletConfig {
                max_tx_fee: Value::MAX,
                funding_pk,
            },
        },
        http: ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: args.http_addr,
                ..Default::default()
            },
        },
        storage: RocksBackendSettings {
            db_path: "./db".into(),
            read_only: false,
            column_family: Some("blocks".into()),
        },
        key_management: PreloadKMSBackendSettings {
            keys: HashMap::from([
                (blend_signing_key_id.clone(), blend_signing_key.into()),
                (blend_zk_key_id.clone(), blend_zk_key.into()),
                (leader_key_id.clone(), leader_key.into()),
                (funding_key_id.clone(), funding_key.into()),
            ]),
        },
        wallet: WalletServiceSettings {
            known_keys: HashMap::from([
                (leader_key_id.clone(), leader_pk),
                (funding_key_id.clone(), funding_pk),
            ]),
            voucher_master_key_id: leader_key_id,
            recovery_path: "./recovery/wallet.json".into(),
        },
        #[cfg(feature = "testing")]
        testing_http: ApiServiceSettings {
            backend_settings: AxumBackendSettings::default(),
        },
    };

    let yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(&args.output, &yaml)?;

    println!("Config written to {}", args.output.display());
    Ok(())
}
