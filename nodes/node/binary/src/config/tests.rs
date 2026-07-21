use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    path::Path,
};

use bytes::Bytes;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_services_utils::overwatch::RecoveryData;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
use tracing::Level;

use crate::{
    UserConfig,
    cli::CliArgs,
    config::{
        DeploymentSettings, RequiredValues as ConfigRequiredValues,
        blend::{
            ServiceConfig as BlendServiceConfig,
            serde::{Config as BlendConfig, RequiredValues as BlendRequiredValues},
        },
        cryptarchia::serde::{
            Config as CryptarchiaConfig, RequiredValues as CryptarchiaRequiredValues,
        },
        mempool::ServiceConfig as MempoolServiceConfig,
        parse_log_filter_layer,
        sdp::{
            ServiceConfig as SdpServiceConfig,
            serde::{Config as SdpConfig, RequiredValues as SdpRequiredValues},
        },
        storage::{
            ServiceConfig as StorageServiceConfig,
            serde::{Config as StorageConfig, RocksDbSettings},
        },
        tracing::serde::{
            console::{Layer as ConsoleLayer, TokioConfig},
            filter::{EnvConfig, Layer},
        },
        wallet::{
            ServiceConfig as WalletServiceConfig,
            serde::{Config as WalletConfig, RequiredValues as WalletRequiredValues},
        },
    },
};

const BLEND_RECOVERY_MARKER: &[u8] = b"recovery/test/blend";
const MEMPOOL_RECOVERY_MARKER: &[u8] = b"recovery/test/mempool";
const SDP_RECOVERY_MARKER: &[u8] = b"recovery/test/sdp";
const WALLET_RECOVERY_MARKER: &[u8] = b"recovery/test/wallet";

fn recovery_data_fixture() -> RecoveryData {
    RecoveryData::new(HashMap::from([
        (BLEND_RECOVERY_MARKER.to_vec(), Bytes::from_static(b"blend")),
        (
            MEMPOOL_RECOVERY_MARKER.to_vec(),
            Bytes::from_static(b"mempool"),
        ),
        (SDP_RECOVERY_MARKER.to_vec(), Bytes::from_static(b"sdp")),
        (
            WALLET_RECOVERY_MARKER.to_vec(),
            Bytes::from_static(b"wallet"),
        ),
    ]))
}

#[test]
fn tokio_console_config_defaults_to_loopback_default_console_port() {
    let config = TokioConfig::default();

    assert_eq!(config.bind_address, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(config.port, 6_669);
    assert_eq!(config.recording_path, None);
}

#[test]
fn tokio_console_config_deserializes() {
    let layer: ConsoleLayer = serde_yaml::from_str(
        "
            !Console
            bind_address: 127.0.0.1
            port: 6669
        ",
    )
    .expect("tokio-console config should deserialize");

    let ConsoleLayer::Console(config) = layer else {
        panic!("expected console layer");
    };

    assert_eq!(config.bind_address, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(config.port, 6_669);
    assert_eq!(config.recording_path, None);
}

#[test]
fn tokio_console_config_deserializes_recording_path_and_converts() {
    let layer: ConsoleLayer = serde_yaml::from_str(
        "
            !Console
            bind_address: 127.0.0.1
            port: 6669
            recording_path: /tmp/node-tokio-console.jsonl
        ",
    )
    .expect("tokio-console config should deserialize");

    let ConsoleLayer::Console(config) = layer else {
        panic!("expected console layer");
    };
    let tracing_config: lb_tracing_service::ConsoleLayerSettings =
        ConsoleLayer::Console(config.clone()).into();
    let lb_tracing_service::ConsoleLayerSettings::Console(tracing_config) = tracing_config else {
        panic!("expected console layer");
    };

    assert_eq!(tracing_config.bind_address, "127.0.0.1");
    assert_eq!(tracing_config.port, 6_669);
    assert_eq!(tracing_config.recording_path, config.recording_path);
}

#[test]
fn parse_config_path() {
    use clap::Parser as _;
    let parsed_args = CliArgs::parse_from(["", "test_cfg.yaml"]);
    assert_eq!(
        parsed_args.user_config_path().to_str().unwrap(),
        "test_cfg.yaml"
    );
}

#[test]
fn service_settings_receive_recovery_data() {
    const STATE_PATH: &str = "./state";
    let recovery_data = recovery_data_fixture();

    let blend_config = BlendConfig::with_required_values(BlendRequiredValues {
        non_ephemeral_signing_key_id: "non_ephemeral_signing_key_id".into(),
        secret_key_kms_id: "secret_key_kms_id".into(),
    });
    let cryptarchia_config = CryptarchiaConfig::with_required_values(CryptarchiaRequiredValues {
        funding_pk: ZkPublicKey::zero(),
    });
    let sdp_config = SdpConfig::with_required_values(SdpRequiredValues {
        funding_pk: ZkPublicKey::zero(),
    });
    let wallet_config = WalletConfig::with_required_values(WalletRequiredValues {
        voucher_master_key_id: "voucher_master_key_id".into(),
    });
    let storage_config = StorageConfig {
        backend: RocksDbSettings {
            folder_name: "db".into(),
            ..RocksDbSettings::default()
        },
    };
    let user_config = {
        let mut base_config = UserConfig::with_required_values(ConfigRequiredValues {
            blend: blend_config,
            cryptarchia: cryptarchia_config,
            sdp: sdp_config,
            wallet: wallet_config,
        });
        base_config.storage = storage_config;
        base_config
    };

    let deployment_settings = DeploymentSettings::default();

    let (blend_service_settings, _, _) = BlendServiceConfig {
        user: user_config.blend.clone(),
        deployment: deployment_settings.blend,
    }
    .into_blend_services_settings(
        recovery_data.clone(),
        &deployment_settings.time,
        &deployment_settings.cryptarchia,
    );
    assert_eq!(
        blend_service_settings
            .common
            .recovery_data
            .take(BLEND_RECOVERY_MARKER)
            .unwrap(),
        Some(Bytes::from_static(b"blend"))
    );

    let wallet_service_settings = WalletServiceConfig {
        user: user_config.wallet.clone(),
    }
    .into_wallet_service_settings(recovery_data.clone());
    assert_eq!(
        wallet_service_settings
            .recovery_data
            .take(WALLET_RECOVERY_MARKER)
            .unwrap(),
        Some(Bytes::from_static(b"wallet"))
    );

    let mempool_service_settings = MempoolServiceConfig {
        deployment: deployment_settings.mempool,
    }
    .into_mempool_service_settings(recovery_data.clone());
    assert_eq!(
        mempool_service_settings
            .recovery_data
            .take(MEMPOOL_RECOVERY_MARKER)
            .unwrap(),
        Some(Bytes::from_static(b"mempool"))
    );

    let sdp_service_settings = SdpServiceConfig {
        user: user_config.sdp.clone(),
    }
    .into_sdp_service_settings(recovery_data);
    assert_eq!(
        sdp_service_settings
            .recovery_data
            .take(SDP_RECOVERY_MARKER)
            .unwrap(),
        Some(Bytes::from_static(b"sdp"))
    );

    let storage_service_settings = StorageServiceConfig {
        user: user_config.storage.clone(),
    }
    .into_rocks_backend_settings(&user_config.state);
    assert!(
        storage_service_settings
            .db_path
            .starts_with(Path::new(STATE_PATH).join("db"))
    );
}

#[test]
fn parse_log_filter_layer_parses_global_and_target_directives() {
    let layer = parse_log_filter_layer("warn,logos_blockchain=debug,libp2p=info")
        .expect("filter should parse");

    let Layer::Env(EnvConfig { filters }) = layer else {
        panic!("expected env filter layer");
    };

    assert_eq!(filters.get("*"), Some(&Level::WARN));
    assert_eq!(filters.get("logos_blockchain"), Some(&Level::DEBUG));
    assert_eq!(filters.get("libp2p"), Some(&Level::INFO));
}

#[test]
fn parse_log_filter_layer_rejects_invalid_level() {
    let error =
        parse_log_filter_layer("logos_blockchain=debgu").expect_err("invalid level should fail");

    assert!(
        error
            .to_string()
            .contains("Invalid log filter level provided: debgu")
    );
}

#[test]
fn parse_log_filter_layer_rejects_empty_directive() {
    let error =
        parse_log_filter_layer("logos_blockchain=").expect_err("empty directive should fail");

    assert!(
        error
            .to_string()
            .contains("Invalid log filter directive: logos_blockchain=")
    );
}

#[test]
fn parse_log_filter_layer_rejects_unknown_blend_target() {
    let error = parse_log_filter_layer("logos_blockchain::blend::service::missing=debug")
        .expect_err("unknown blend target should fail");

    assert!(
        error
            .to_string()
            .contains("unknown log filter target `logos_blockchain::blend::service::missing`")
    );
}

#[test]
fn env_config_serializes_and_deserializes_typed_levels() {
    let config = EnvConfig {
        filters: [
            ("*".to_owned(), Level::WARN),
            ("logos_blockchain".to_owned(), Level::DEBUG),
            ("libp2p".to_owned(), Level::INFO),
        ]
        .into_iter()
        .collect(),
    };

    let json = serde_json::to_string(&config).expect("serialize env config");
    let decoded: EnvConfig = serde_json::from_str(&json).expect("deserialize env config");

    assert_eq!(decoded.filters, config.filters);
}

#[test]
fn env_config_deserialization_rejects_invalid_level() {
    let error = serde_json::from_str::<EnvConfig>(r#"{"filters":{"logos_blockchain":"debgu"}}"#)
        .expect_err("invalid level should fail");

    assert!(
        error
            .to_string()
            .contains("Invalid log filter level provided: debgu")
    );
}

#[test]
fn env_config_deserialization_rejects_unknown_blend_target() {
    let error = serde_json::from_str::<EnvConfig>(
        r#"{"filters":{"logos_blockchain::blend::service::missing":"debug"}}"#,
    )
    .expect_err("unknown blend target should fail");

    assert!(
        error
            .to_string()
            .contains("unknown log filter target `logos_blockchain::blend::service::missing`")
    );
}

fn repo_file(path_from_crate_root: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path_from_crate_root)
}

#[test]
fn standalone_node_config_deserializes() {
    let yaml_path = repo_file("../standalone-node-config.yaml");
    assert!(
        yaml_path.exists(),
        "standalone node config should exist at {yaml_path:?}"
    );
    let bytes = std::fs::read(&yaml_path).expect("standalone node config should exist");

    let parsed: Result<UserConfig, serde_yaml::Error> = serde_yaml::from_slice(&bytes);
    assert!(parsed.is_ok(), "standalone node config should deserialize");

    let parsed = deserialize_value_at_path::<UserConfig>(&yaml_path, OnUnknownKeys::Fail);
    assert!(
        parsed.is_ok(),
        "standalone node config should deserialize via loader, got: {:?}",
        parsed.err()
    );
}

#[test]
fn standalone_deployment_config_deserializes() {
    let yaml_path = repo_file("../standalone-deployment-config.yaml");
    assert!(
        yaml_path.exists(),
        "standalone deployment config should exist at {yaml_path:?}"
    );
    let bytes = std::fs::read(&yaml_path).expect("standalone deployment config should exist");

    let parsed: Result<DeploymentSettings, serde_yaml::Error> = serde_yaml::from_slice(&bytes);
    assert!(
        parsed.is_ok(),
        "standalone deployment config should deserialize ({:?})",
        parsed.err(),
    );

    let parsed = deserialize_value_at_path::<DeploymentSettings>(&yaml_path, OnUnknownKeys::Fail);
    assert!(
        parsed.is_ok(),
        "standalone deployment config should deserialize via loader, got: {:?}",
        parsed.err()
    );
}
