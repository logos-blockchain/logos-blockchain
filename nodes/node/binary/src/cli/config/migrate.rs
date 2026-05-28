use color_eyre::eyre::Result;
use thiserror::Error;

use crate::cli::{
    MigrateArgs,
    config::{init::build_user_config, keystore::Keystore},
};

#[derive(Error, Debug)]
pub enum MigrateError {
    #[error("Update command cancelled by user.")]
    UserCancelled,

    #[error("User configuration exists. Use `update` command.")]
    UserFileExists,

    #[error("Keystore file does not exist. Use `init` command.")]
    KeystoreFileDoesNotExist,
}

pub fn run(args: MigrateArgs) -> Result<()> {
    let user_config_path = args.output.clone();
    let keystore_path = args.keystore.clone();

    if user_config_path.exists() {
        return Err(MigrateError::UserFileExists.into());
    }

    if !keystore_path.exists() {
        return Err(MigrateError::KeystoreFileDoesNotExist.into());
    }

    let keystore_yaml = std::fs::read_to_string(&keystore_path)?;
    let keystore: Keystore = serde_yaml::from_str(&keystore_yaml)?;

    let user_config = build_user_config(&keystore, args.into());

    let user_config_yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(&user_config_path, &user_config_yaml)?;

    Ok(())
}
