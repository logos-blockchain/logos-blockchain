use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use color_eyre::eyre::Result;
use thiserror::Error;

use crate::{
    UserConfig,
    cli::{
        UpdateArgs,
        config::{keystore::Keystore, update::update_user_config},
    },
};

#[derive(Error, Debug)]
pub enum KeysError {
    #[error("Update command cancelled by user.")]
    UserCancelled,

    #[error("User configuration does not exist.")]
    UserFileDoesNotExist,

    #[error("Keystore file does not exist.")]
    KeystoreFileDoesNotExist,
}

#[derive(ValueEnum, Debug, Clone, PartialEq, Eq)]
#[clap(rename_all = "lower")]
pub enum KeyType {
    Ed25519,
    Zk,
}

#[derive(Parser, Debug)]
pub struct GenerateKeyArgs {
    /// Path for the user config file.
    #[clap(long = "user_config", short = 'c', default_value = "user_config.yaml")]
    user_config: PathBuf,

    /// Path for the keystore file.
    #[clap(long = "keystore", short = 'k', default_value = "keystore.yaml")]
    keystore: PathBuf,

    /// Auto approve interactive promps.
    #[arg(long, short, default_value_t = false)]
    yes: bool,

    #[arg(long = "key-type", short = 't')]
    key_type: KeyType,
}

pub fn run_generate_key(args: &GenerateKeyArgs) -> Result<()> {
    let GenerateKeyArgs {
        user_config: user_config_path,
        keystore: keystore_path,
        key_type,
        yes: auto_approve,
    } = args;

    if !user_config_path.exists() {
        return Err(KeysError::UserFileDoesNotExist.into());
    }

    if !keystore_path.exists() {
        return Err(KeysError::KeystoreFileDoesNotExist.into());
    }

    let user_config_yaml = std::fs::read_to_string(user_config_path)?;
    let mut user_config: UserConfig = serde_yaml::from_str(&user_config_yaml)?;

    let keystore_yaml = std::fs::read_to_string(keystore_path)?;
    let mut keystore: Keystore = serde_yaml::from_str(&keystore_yaml)?;

    update_user_config(&mut user_config, &keystore, UpdateArgs::default());

    let user_config_yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(user_config_path, &user_config_yaml)?;

    let keystore_yaml = serde_yaml::to_string(&keystore)?;
    std::fs::write(keystore_path, &keystore_yaml)?;

    Ok(())
}
