use std::path::Path;

use clap::Parser as _;
use lb_key_management_system_service::keys::ZkPublicKey;
use tracing::Level;

use crate::{
    UserConfig,
    config::{
        CliArgs, DeploymentSettings, RequiredValues as ConfigRequiredValues, WellKnownDeployment,
        blend::{
            ServiceConfig as BlendServiceConfig,
            serde::{Config as BlendConfig, RequiredValues as BlendRequiredValues},
        },
        cryptarchia::{
            ServiceConfig as CryptarchiaServiceConfig,
            serde::{Config as CryptarchiaConfig, RequiredValues as CryptarchiaRequiredValues},
        },
        mempool::ServiceConfig as MempoolServiceConfig,
        parse_log_filter_layer,
        sdp::serde::{Config as SdpConfig, RequiredValues as SdpRequiredValues},
        storage::{
            ServiceConfig as StorageServiceConfig,
            serde::{Config as StorageConfig, RocksDbSettings},
        },
        tracing::serde::filter::{EnvConfig, Layer},
        wallet::{
            ServiceConfig as WalletServiceConfig,
            serde::{Config as WalletConfig, RequiredValues as WalletRequiredValues},
        },
    },
};

#[test]
fn parse_config_path() {
    let parsed_args = CliArgs::parse_from(["", "test_cfg.yaml"]);
    assert_eq!(parsed_args.config_path().to_str().unwrap(), "test_cfg.yaml");
}

#[test]
fn common_recovery_folder() {
    const STATE_PATH: &str = "./state";

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

    let deployment_settings = DeploymentSettings::from(WellKnownDeployment::Devnet);

    let blend_rewards_params = deployment_settings.blend_reward_params();

    let (blend_service_settings, _, _) = BlendServiceConfig {
        user: user_config.blend.clone(),
        deployment: deployment_settings.blend,
    }
    .into_blend_services_settings(
        &user_config.state,
        &deployment_settings.time,
        &deployment_settings.cryptarchia,
    );
    assert!(
        blend_service_settings
            .common
            .recovery_path_prefix
            .starts_with(Path::new(STATE_PATH).join("recovery").join("blend"))
    );

    let (chain_service_settings, _, _) = CryptarchiaServiceConfig {
        user: user_config.cryptarchia.clone(),
        deployment: deployment_settings.cryptarchia,
    }
    .into_cryptarchia_services_settings(blend_rewards_params, &user_config.state);
    assert!(
        chain_service_settings
            .recovery_file
            .starts_with(Path::new(STATE_PATH).join("recovery").join("consensus"))
    );

    let wallet_service_settings = WalletServiceConfig {
        user: user_config.wallet.clone(),
    }
    .into_wallet_service_settings(&user_config.state);
    assert!(
        wallet_service_settings
            .recovery_path
            .starts_with(Path::new(STATE_PATH).join("recovery").join("wallet"))
    );

    let mempool_service_settings = MempoolServiceConfig {
        deployment: deployment_settings.mempool,
    }
    .into_mempool_service_settings(&user_config.state);
    assert!(
        mempool_service_settings
            .recovery_path
            .starts_with(Path::new(STATE_PATH).join("recovery").join("mempool"))
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

    assert!(error.to_string().contains("invalid log level"));
}
