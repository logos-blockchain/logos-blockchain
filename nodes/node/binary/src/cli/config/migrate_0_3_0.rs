//! Migration of a node of v0.3.0 to the HD wallet
//!
//! The keys of v0.3.0 are kept as static keys, so that the funds they hold stay
//! spendable. A mnemonic is added, which the new keys of the wallet are
//! derived from.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use clap::Parser;
use color_eyre::eyre::{Result, WrapErr as _, eyre};
use lb_key_management_system_keys::{
    hd::{Mnemonic, Passphrase},
    keys::Key,
};
use lb_key_management_system_service::backend::hd_and_preload::KeyId;
use lb_storage_service::{
    backend::StorageBackend as _,
    recovery::recovery_key,
    rocksdb::{RocksBackend, RocksBackendSettings},
};
use lb_wallet_service::migrate_from_0_3_0::migrate_recovery_state;
use rand::rngs::OsRng;
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use thiserror::Error;

use crate::{
    UserConfig,
    cli::config::{
        confirm_overwrite,
        keystore::{KeyTitle, Keystore},
    },
    config::storage::ServiceConfig as StorageConfig,
};

#[cfg(test)]
mod tests;

#[derive(Parser, Debug)]
pub struct MigrateArgs {
    /// The user config of v0.3.0, which is overwritten.
    #[clap(long = "user-config", short = 'u', default_value = "user_config.yaml")]
    pub user_config: PathBuf,

    /// The keystore of v0.3.0, which is overwritten.
    #[clap(long = "keystore", default_value = "keystore.yaml")]
    pub keystore: PathBuf,

    /// BIP-39 mnemonic to derive the new wallet keys from.
    /// A new 12-word mnemonic is generated if not given.
    #[clap(long = "mnemonic", env = "MNEMONIC")]
    pub mnemonic: Option<Mnemonic>,

    /// BIP-39 passphrase of the mnemonic, which is empty if not given.
    #[clap(long = "mnemonic-passphrase", env = "MNEMONIC_PASSPHRASE")]
    pub mnemonic_passphrase: Option<Passphrase>,

    /// Auto approve interactive promps.
    #[arg(short, long, default_value_t = false)]
    pub auto_approve: bool,
}

#[derive(Error, Debug)]
enum MigrateError {
    #[error("Migration cancelled by user.")]
    UserCancelled,

    #[error("'{0}' is not a user config of v0.3.0: {1}")]
    NotUserConfig(PathBuf, String),
}

/// The keystore of v0.3.0
#[derive(Deserialize)]
struct KeystoreV0_3_0 {
    secret_keys: HashMap<KeyTitle, Key>,
}

pub async fn run(args: MigrateArgs) -> Result<()> {
    let MigrateArgs {
        user_config: user_config_path,
        keystore: keystore_path,
        mnemonic,
        mnemonic_passphrase,
        auto_approve,
    } = args;

    // Everything is converted before anything is written.
    let old_keystore: KeystoreV0_3_0 = serde_yaml::from_str(&fs::read_to_string(&keystore_path)?)
        .wrap_err_with(|| {
        format!("'{}' is not a keystore of v0.3.0", keystore_path.display())
    })?;
    let keystore = migrate_keystore(
        old_keystore,
        mnemonic.unwrap_or_else(|| Mnemonic::generate(&mut OsRng)),
        mnemonic_passphrase,
    );

    let old_user_config: Value = serde_yaml::from_str(&fs::read_to_string(&user_config_path)?)?;
    let user_config = migrate_user_config(old_user_config, &keystore)
        .map_err(|error| MigrateError::NotUserConfig(user_config_path.clone(), error))?;

    let db_settings = StorageConfig {
        user: user_config.storage.clone(),
    }
    .into_rocks_backend_settings(&user_config.state);

    if !auto_approve
        && !confirm_overwrite(&format!(
            "The user config, the keystore and the DB at '{}' will be migrated. \
             Back up the DB first if you want to keep a copy of it. Continue?",
            db_settings.db_path.display()
        ))?
    {
        return Err(MigrateError::UserCancelled.into());
    }

    migrate_db(db_settings).await?;

    backup(&user_config_path)?;
    backup(&keystore_path)?;
    fs::write(&user_config_path, serde_yaml::to_string(&user_config)?)?;
    fs::write(&keystore_path, serde_yaml::to_string(&keystore)?)?;

    println!(
        "Migrated. Back up the new mnemonic and the keystore, which still holds the keys of \
         v0.3.0. Keep the `Legacy` keys as long as they hold funds. The files of v0.3.0 are \
         kept with the `.v0.3.0` extension."
    );
    Ok(())
}

/// Titles of the keys that a keystore of v0.3.0 holds besides the predefined
/// ones, with the titles they are renamed to.
const LEGACY_TITLES: [(&str, &str); 5] = [
    ("LeaderFunding", KeyTitle::LEGACY_LEADER_FUNDING),
    ("PoWClaim", KeyTitle::LEGACY_POW_CLAIM),
    ("SdpFunding", KeyTitle::LEGACY_SDP_FUNDING),
    ("Stake", KeyTitle::LEGACY_STAKE),
    ("VaucherMaster", KeyTitle::LEGACY_VOUCHER_MASTER),
];

/// Converts the keystore of v0.3.0, whose keys are kept as static keys.
fn migrate_keystore(
    old_keystore: KeystoreV0_3_0,
    mnemonic: Mnemonic,
    passphrase: Option<Passphrase>,
) -> Keystore {
    let static_keys = old_keystore
        .secret_keys
        .into_iter()
        .map(|(title, key)| {
            let title = LEGACY_TITLES
                .iter()
                .find(|(old, _)| title.0 == *old)
                .map_or(title, |(_, new)| KeyTitle::from(*new));
            (title, key)
        })
        .collect();
    Keystore::from_static_keys(mnemonic, passphrase, static_keys)
}

/// Converts the user config of v0.3.0.
///
/// The keys of v0.3.0 are kept as static keys, and the stake key of the
/// keystore becomes unspendable. The leader and SDP services lose their
/// funding key, since they fund from all the keys of the wallet.
fn migrate_user_config(mut config: Value, keystore: &Keystore) -> Result<UserConfig, String> {
    let kms = mapping_at(&mut config, &["kms", "backend"])?;
    rename(kms, "keys", "static_keys");
    let kms_backend = keystore.kms_backend_settings();
    kms.insert(
        "mnemonic".into(),
        serde_yaml::to_value(&kms_backend.mnemonic).map_err(|e| e.to_string())?,
    );
    if let Some(passphrase) = &kms_backend.passphrase {
        kms.insert(
            "passphrase".into(),
            serde_yaml::to_value(passphrase).map_err(|e| e.to_string())?,
        );
    }

    let wallet = mapping_at(&mut config, &["wallet"])?;
    rename(wallet, "known_keys", "static_keys");
    // The vouchers created so far keep the id of the voucher master key they
    // are derived from. The new ones are derived from the HD key.
    wallet.remove("voucher_master_key_id");
    wallet.insert(
        "unspendable_keys".into(),
        serde_yaml::to_value(keystore.unspendable_public_keys()).map_err(|e| e.to_string())?,
    );

    tag_static_key_id(
        mapping_at(&mut config, &["blend"])?,
        "non_ephemeral_signing_key_id",
    )?;
    tag_static_key_id(
        mapping_at(&mut config, &["blend", "core", "zk"])?,
        "secret_key_kms_id",
    )?;

    for path in [&["cryptarchia", "leader", "wallet"][..], &["sdp", "wallet"]] {
        if let Ok(section) = mapping_at(&mut config, path) {
            section.remove("funding_pk");
        }
    }

    serde_yaml::from_value(config).map_err(|e| e.to_string())
}

/// Converts the recovery state of the wallet, if the node has run.
async fn migrate_db(settings: RocksBackendSettings) -> Result<()> {
    if !settings.db_path.exists() {
        return Ok(());
    }

    let mut db = RocksBackend::new(settings)?;
    let key = recovery_key(b"wallet");
    if let Some(state) = db.load(&key).await? {
        let state = migrate_recovery_state(&state)
            .map_err(|e| eyre!("Failed to migrate the wallet state: {e}"))?;
        db.store(key, state).await?;
    }
    Ok(())
}

/// Copies the file to the same path with the `.v0.3.0` extension.
fn backup(path: &Path) -> Result<()> {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".v0.3.0");
    fs::copy(path, &backup)?;
    Ok(())
}

fn mapping_at<'a>(value: &'a mut Value, path: &[&str]) -> Result<&'a mut Mapping, String> {
    let mut value = value;
    for key in path {
        value = value
            .get_mut(*key)
            .ok_or_else(|| format!("'{}' is missing", path.join(".")))?;
    }
    value
        .as_mapping_mut()
        .ok_or_else(|| format!("'{}' is not a mapping", path.join(".")))
}

fn rename(mapping: &mut Mapping, from: &str, to: &str) {
    if let Some(value) = mapping.remove(from) {
        mapping.insert(to.into(), value);
    }
}

/// Tags the key id of the preload KMS backend under `key` as a static key id.
fn tag_static_key_id(mapping: &mut Mapping, key: &str) -> Result<(), String> {
    let Some(Value::String(key_id)) = mapping.get(key) else {
        return Err(format!("'{key}' is not the id of a key of v0.3.0"));
    };
    let key_id = serde_yaml::to_value(KeyId::Static(key_id.clone())).map_err(|e| e.to_string())?;
    mapping.insert(key.into(), key_id);
    Ok(())
}
