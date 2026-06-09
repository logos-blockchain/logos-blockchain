use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use color_eyre::eyre::Result;
use lb_key_management_system_service::keys::{
    Ed25519Key, Key, UnsecuredEd25519Key, UnsecuredZkKey, ZkKey,
};
use thiserror::Error;

use crate::{
    UserConfig,
    cli::{
        UpdateArgs,
        config::{
            confirm_overwrite,
            keystore::{KeyTitle, Keystore},
            update::update_user_config,
        },
    },
    config::{parse_hex_ed25519_key, parse_hex_zk_key},
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

    #[clap(long = "title")]
    pub key_title: Option<String>,

    #[arg(long = "key-type", short = 't')]
    key_type: KeyType,
}

#[derive(Parser, Debug)]
pub struct AddKeyArgs {
    /// Path for the user config file.
    #[clap(long = "user_config", short = 'c', default_value = "user_config.yaml")]
    user_config: PathBuf,

    /// Path for the keystore file.
    #[clap(long = "keystore", short = 'k', default_value = "keystore.yaml")]
    keystore: PathBuf,

    /// Auto approve interactive promps.
    #[arg(long, short, default_value_t = false)]
    yes: bool,

    #[clap(long = "title")]
    pub key_title: Option<String>,

    #[clap(
        long = "zk",
        value_parser = parse_hex_zk_key
    )]
    pub zk_key: Option<UnsecuredZkKey>,

    #[clap(
        long = "ed25519",
        value_parser = parse_hex_ed25519_key
    )]
    pub ed25519_key: Option<UnsecuredEd25519Key>,
}

#[derive(Parser, Debug)]
pub struct GetKeyArgs {
    /// Path for the user config file.
    #[clap(long = "user_config", short = 'c', default_value = "user_config.yaml")]
    user_config: PathBuf,

    /// Path for the keystore file.
    #[clap(long = "keystore", short = 'k', default_value = "keystore.yaml")]
    keystore: PathBuf,

    #[clap(long = "title")]
    pub key_title: String,
}

#[derive(Parser, Debug)]
pub struct RemoveKeyArgs {
    /// Path for the user config file.
    #[clap(long = "user_config", short = 'c', default_value = "user_config.yaml")]
    user_config: PathBuf,

    /// Path for the keystore file.
    #[clap(long = "keystore", short = 'k', default_value = "keystore.yaml")]
    keystore: PathBuf,

    /// Auto approve interactive promps.
    #[arg(long, short, default_value_t = false)]
    yes: bool,

    #[clap(long = "title")]
    pub key_title: String,
}

pub fn run_generate_key(args: GenerateKeyArgs) -> Result<()> {
    let GenerateKeyArgs {
        user_config: user_config_path,
        keystore: keystore_path,
        key_title,
        key_type,
        yes: auto_approve,
    } = args;

    if !user_config_path.exists() {
        return Err(KeysError::UserFileDoesNotExist.into());
    }

    if !keystore_path.exists() {
        return Err(KeysError::KeystoreFileDoesNotExist.into());
    }

    let user_config_yaml = std::fs::read_to_string(&user_config_path)?;
    let mut user_config: UserConfig = serde_yaml::from_str(&user_config_yaml)?;

    let keystore_yaml = std::fs::read_to_string(&keystore_path)?;
    let mut keystore: Keystore = serde_yaml::from_str(&keystore_yaml)?;

    let user_key_title = key_title
        .as_ref()
        .map_or_else(|| next_user_key_title(&keystore), Clone::clone);

    let (key_id, key): (_, Key) = match key_type {
        KeyType::Ed25519 => {
            let (id, secret_key) = keystore.generate_ed25519(user_key_title);
            (id, Ed25519Key::from(secret_key).into())
        }
        KeyType::Zk => {
            let (id, secret_key) = keystore.generate_zk(user_key_title);
            (id, ZkKey::from(secret_key).into())
        }
    };

    if !auto_approve && confirm_overwrite("Write key to keystore?")? {
        update_user_config(&mut user_config, &keystore, UpdateArgs::default());

        let user_config_yaml = serde_yaml::to_string(&user_config)?;
        std::fs::write(&user_config_path, &user_config_yaml)?;

        let keystore_yaml = serde_yaml::to_string(&keystore)?;
        std::fs::write(&keystore_path, &keystore_yaml)?;
    } else {
        println!("KeyID: {key_id}");
        println!("Key: {key:?}");
    }

    Ok(())
}

pub fn run_add_key(args: AddKeyArgs) -> Result<()> {
    let AddKeyArgs {
        user_config: user_config_path,
        keystore: keystore_path,
        yes: auto_approve,
        key_title,
        zk_key,
        ed25519_key,
    } = args;

    if !user_config_path.exists() {
        return Err(KeysError::UserFileDoesNotExist.into());
    }

    if !keystore_path.exists() {
        return Err(KeysError::KeystoreFileDoesNotExist.into());
    }

    let user_config_yaml = std::fs::read_to_string(&user_config_path)?;
    let mut user_config: UserConfig = serde_yaml::from_str(&user_config_yaml)?;

    let keystore_yaml = std::fs::read_to_string(&keystore_path)?;
    let mut keystore: Keystore = serde_yaml::from_str(&keystore_yaml)?;

    let user_key_title = key_title
        .as_ref()
        .map_or_else(|| next_user_key_title(&keystore), Clone::clone);

    let key: Key = match (ed25519_key, zk_key) {
        (Some(ed_secret), None) => {
            let ed_key = Ed25519Key::from(ed_secret);
            Key::Ed25519(ed_key)
        }
        (None, Some(zk_sercret)) => {
            let zk_key = ZkKey::from(zk_sercret);
            Key::Zk(zk_key)
        }
        (Some(_), Some(_)) => {
            return Err(color_eyre::eyre::eyre!(
                "Please provide either --ed25519 or --zk, not both."
            ));
        }
        (None, None) => {
            return Err(color_eyre::eyre::eyre!(
                "You must provide a key via --ed25519 or --zk."
            ));
        }
    };

    keystore.set(user_key_title.clone(), key);

    if auto_approve || confirm_overwrite(&format!("Add key '{user_key_title}' to keystore?"))? {
        update_user_config(&mut user_config, &keystore, UpdateArgs::default());

        let user_config_yaml = serde_yaml::to_string(&user_config)?;
        std::fs::write(&user_config_path, &user_config_yaml)?;

        let keystore_yaml = serde_yaml::to_string(&keystore)?;
        std::fs::write(&keystore_path, &keystore_yaml)?;

        println!("Successfully added key '{user_key_title}' to files.");
    } else {
        println!("Action discarded.");
    }

    Ok(())
}

pub fn run_remove_key(args: RemoveKeyArgs) -> Result<()> {
    let RemoveKeyArgs {
        user_config: user_config_path,
        keystore: keystore_path,
        yes: auto_approve,
        key_title,
    } = args;

    if !user_config_path.exists() {
        return Err(KeysError::UserFileDoesNotExist.into());
    }

    if !keystore_path.exists() {
        return Err(KeysError::KeystoreFileDoesNotExist.into());
    }

    let user_config_yaml = std::fs::read_to_string(&user_config_path)?;
    let mut user_config: UserConfig = serde_yaml::from_str(&user_config_yaml)?;

    let keystore_yaml = std::fs::read_to_string(&keystore_path)?;
    let mut keystore: Keystore = serde_yaml::from_str(&keystore_yaml)?;

    let title_key = KeyTitle::from(key_title.clone());

    if keystore.get(title_key.clone()).is_none() {
        return Err(crate::cli::config::keystore::KeystoreError::NotFound(title_key).into());
    }

    if auto_approve
        || confirm_overwrite(&format!(
            "Are you sure you want to remove the key '{key_title}'?"
        ))?
    {
        keystore.remove(title_key);

        update_user_config(&mut user_config, &keystore, UpdateArgs::default());

        let user_config_yaml = serde_yaml::to_string(&user_config)?;
        std::fs::write(&user_config_path, &user_config_yaml)?;

        let keystore_yaml = serde_yaml::to_string(&keystore)?;
        std::fs::write(&keystore_path, &keystore_yaml)?;

        println!("Successfully removed key '{key_title}' from files.");
    } else {
        return Err(KeysError::UserCancelled.into());
    }

    Ok(())
}

fn next_user_key_title(keystore: &Keystore) -> String {
    let mut counter = 1;
    loop {
        let candidate = format!("UserKey{counter}");
        let candidate_title = KeyTitle::from(candidate.clone());
        if keystore.get(candidate_title).is_none() {
            break candidate;
        }
        counter += 1;
    }
}
