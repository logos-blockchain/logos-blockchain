use std::path::Path;

use clap::Parser as _;
use lb_key_management_system_service::keys::ZkPublicKey;

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
        sdp::serde::{Config as SdpConfig, RequiredValues as SdpRequiredValues},
        storage::{
            ServiceConfig as StorageServiceConfig,
            serde::{Config as StorageConfig, RocksDbSettings},
        },
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

    let (chain_service_settings, _, _) = CryptarchiaServiceConfig {
        user: user_config.cryptarchia.clone(),
        deployment: deployment_settings.cryptarchia,
    }
    .into_cryptarchia_services_settings(&deployment_settings.blend, &user_config.state);
    assert!(
        chain_service_settings
            .recovery_file
            .starts_with(Path::new(STATE_PATH).join("recovery").join("consensus"))
    );

    let (blend_service_settings, _, _) = BlendServiceConfig {
        user: user_config.blend.clone(),
        deployment: deployment_settings.blend,
    }
    .into_blend_services_settings(&user_config.state);
    assert!(
        blend_service_settings
            .common
            .recovery_path_prefix
            .starts_with(Path::new(STATE_PATH).join("recovery").join("blend"))
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
