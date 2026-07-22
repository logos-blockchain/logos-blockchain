pub mod config;

use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::Result;
use thiserror::Error;

use crate::{
    ApiArgs, LogArgs, NetworkArgs,
    cli::{
        InitArgs,
        config::{init::build_user_config, migrate_0_1_2::config::OldConfig},
    },
    config::{BlendArgs, CryptarchiaArgs, SdpArgs, StateArgs},
};

#[derive(Parser, Debug)]
pub struct MigrateArgs {
    /// Output file path for the generated config.
    #[clap(long = "new-config", default_value = "user_config_new.yaml")]
    pub new_config: PathBuf,

    /// Output file path for the old config.
    #[clap(long = "old-config", default_value = "user_config.yaml")]
    pub old_config: PathBuf,

    /// Path for the keystore file.
    #[clap(long = "keystore", default_value = "keystore.yaml")]
    pub keystore: PathBuf,

    #[clap(flatten)]
    log: LogArgs,

    #[clap(flatten)]
    network: NetworkArgs,

    #[clap(flatten)]
    blend: BlendArgs,

    #[clap(flatten)]
    cryptarchia: CryptarchiaArgs,

    #[clap(flatten)]
    sdp: SdpArgs,

    #[clap(flatten)]
    api: ApiArgs,

    #[clap(flatten)]
    state: StateArgs,
}

impl MigrateArgs {
    /// Creates arguments programmatically (e.g. from the c-bindings crate),
    /// leaving all config overrides at their defaults.
    #[must_use]
    pub fn new(new_config: PathBuf, old_config: PathBuf, keystore: PathBuf) -> Self {
        Self {
            new_config,
            old_config,
            keystore,
            log: LogArgs::default(),
            network: NetworkArgs::default(),
            blend: BlendArgs::default(),
            cryptarchia: CryptarchiaArgs::default(),
            sdp: SdpArgs::default(),
            api: ApiArgs::default(),
            state: StateArgs::default(),
        }
    }
}

impl From<MigrateArgs> for InitArgs {
    fn from(migrate: MigrateArgs) -> Self {
        Self {
            output: migrate.new_config,
            keystore: Some(migrate.keystore),
            log: migrate.log,
            network: migrate.network,
            blend: migrate.blend,
            cryptarchia: migrate.cryptarchia,
            sdp: migrate.sdp,
            api: migrate.api,
            state: migrate.state,
            storage_path: None,
            overwrite: false,
        }
    }
}

#[derive(Error, Debug)]
pub enum MigrateError {
    #[error("Update command cancelled by user.")]
    UserCancelled,

    #[error("User configuration exists. Use `update` command.")]
    UserFileExists,

    #[error("Keystore file already exists. Cannot overwrite during migration.")]
    KeystoreFileExists,

    #[error("Old configuration file does not exist at the specified path.")]
    OldConfigFileDoesNotExist,
}

pub fn run(args: MigrateArgs) -> Result<()> {
    let user_config_path = args.new_config.clone();
    let old_config_path = args.old_config.clone();
    let keystore_path = args.keystore.clone();

    if user_config_path.exists() {
        return Err(MigrateError::UserFileExists.into());
    }

    if !old_config_path.exists() {
        return Err(MigrateError::OldConfigFileDoesNotExist.into());
    }

    if keystore_path.exists() {
        return Err(MigrateError::KeystoreFileExists.into());
    }

    let old_config_yaml = std::fs::read_to_string(&old_config_path)?;
    let old_config: OldConfig = serde_yaml::from_str(&old_config_yaml)?;

    let keystore = old_config.into_keystore();

    let keystore_yaml = serde_yaml::to_string(&keystore)?;
    std::fs::write(&keystore_path, &keystore_yaml)?;

    let user_config = build_user_config(&keystore, args.into());
    let user_config_yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(&user_config_path, &user_config_yaml)?;

    Ok(())
}
