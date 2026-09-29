//! The upgrade of the files of a node that has no HD wallet
//!
//! Such a node lists every key that it uses, under the hex of its public key.
//! The upgrade gives the node a mnemonic to derive the keys of its wallet
//! from, and keeps the keys that the node has under their titles: the node
//! still leads with the notes of its stake key and claims the vouchers of its
//! voucher master key.
//!
//! The funds of the node are not moved. Its wallet does not spend the notes of
//! the keys that are not derived from the mnemonic, unless it is told to.

mod keystore;
mod state;
#[cfg(test)]
mod tests;
mod user_config;

use std::path::{Path, PathBuf};

use clap::Parser;
use color_eyre::eyre::{Result, bail, eyre};
use lb_key_management_system_service::hd::{Mnemonic, Passphrase};
use rand::rngs::OsRng;
use serde_yaml::Value;

use crate::{
    UserConfig,
    cli::{
        addresses::addresses_from_config, config::confirm_overwrite,
        upgrade::keystore::LegacyKeystore,
    },
};

#[derive(Parser, Debug)]
pub struct UpgradeArgs {
    /// Path of the user config file to upgrade.
    #[clap(long = "user-config", short = 'u', default_value = "user_config.yaml")]
    pub user_config: PathBuf,

    /// Path of the keystore file to upgrade.
    #[clap(long = "keystore", short = 'k', default_value = "keystore.yaml")]
    pub keystore: PathBuf,

    /// BIP-39 mnemonic to derive the wallet keys from.
    /// A new 12-word mnemonic is generated if not given.
    #[clap(long = "mnemonic", env = "MNEMONIC")]
    pub mnemonic: Option<Mnemonic>,

    /// BIP-39 passphrase of the mnemonic, which is empty if not given.
    #[clap(long = "mnemonic-passphrase", env = "MNEMONIC_PASSPHRASE")]
    pub mnemonic_passphrase: Option<Passphrase>,

    /// Auto approve interactive prompts.
    #[arg(long, short, default_value_t = false)]
    pub yes: bool,
}

impl UpgradeArgs {
    /// Creates arguments programmatically (e.g. from the c-bindings crate),
    /// which skip interactive prompts.
    #[must_use]
    pub const fn new(
        user_config: PathBuf,
        keystore: PathBuf,
        mnemonic: Option<Mnemonic>,
        mnemonic_passphrase: Option<Passphrase>,
    ) -> Self {
        Self {
            user_config,
            keystore,
            mnemonic,
            mnemonic_passphrase,
            yes: true,
        }
    }
}

pub fn run(args: UpgradeArgs) -> Result<()> {
    let UpgradeArgs {
        user_config: user_config_path,
        keystore: keystore_path,
        mnemonic,
        mnemonic_passphrase,
        yes,
    } = args;

    let mut user_config: Value =
        serde_yaml::from_str(&std::fs::read_to_string(&user_config_path)?)?;
    if user_config::is_upgraded(&user_config) {
        println!("The node has an HD wallet already.");
        return Ok(());
    }
    let legacy_keystore: LegacyKeystore =
        serde_yaml::from_str(&std::fs::read_to_string(&keystore_path)?)?;

    if !yes
        && !confirm_overwrite(
            "The node has to be stopped and its database backed up. Do you want to upgrade it?",
        )?
    {
        bail!("Upgrade cancelled by user.");
    }

    let mnemonic = mnemonic.unwrap_or_else(|| Mnemonic::generate(&mut OsRng));
    let key_titles = legacy_keystore.titles_by_key_id();
    let keystore = legacy_keystore.upgrade(mnemonic, mnemonic_passphrase);

    user_config::upgrade(&mut user_config, &keystore, &key_titles)?;
    let upgraded_user_config: UserConfig = serde_yaml::from_value(user_config.clone())
        .map_err(|error| eyre!("The upgraded user config is not valid: {error}"))?;

    // The files are written next to the ones that they replace, for the
    // database not to be upgraded if they cannot be written.
    let new_user_config_path = with_suffix(&user_config_path, "new");
    let new_keystore_path = with_suffix(&keystore_path, "new");
    std::fs::write(&new_user_config_path, serde_yaml::to_string(&user_config)?)?;
    std::fs::write(&new_keystore_path, serde_yaml::to_string(&keystore)?)?;
    std::fs::copy(&user_config_path, with_suffix(&user_config_path, "bak"))?;
    std::fs::copy(&keystore_path, with_suffix(&keystore_path, "bak"))?;

    if let Err(error) = state::upgrade(&upgraded_user_config) {
        std::fs::remove_file(&new_user_config_path)?;
        std::fs::remove_file(&new_keystore_path)?;
        return Err(error);
    }

    std::fs::rename(&new_user_config_path, &user_config_path)?;
    std::fs::rename(&new_keystore_path, &keystore_path)?;

    println!(
        "Upgraded. The mnemonic is in '{}': back it up.",
        keystore_path.display()
    );
    for address in addresses_from_config(&upgraded_user_config) {
        println!("{address}");
    }
    println!(
        "The wallet pays fees from its receive address. Transfer the funds of the node to it."
    );

    Ok(())
}

/// The path of the file, with the suffix added to its name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    name.into()
}
