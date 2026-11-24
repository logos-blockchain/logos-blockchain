use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use color_eyre::eyre::Result;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Ed25519Key, Key, UnsecuredEd25519Key, UnsecuredZkKey, ZkKey},
};
use thiserror::Error;

use crate::{
    cli::config::confirm_overwrite,
    config::{
        keystore::{KeyTitle, Keystore, KeystoreError},
        parse_hex_ed25519_key, parse_hex_zk_key,
    },
};

#[derive(Error, Debug)]
pub enum KeysError {
    #[error("Update command cancelled by user.")]
    UserCancelled,
}

#[derive(ValueEnum, Debug, Clone, PartialEq, Eq)]
#[clap(rename_all = "lower")]
pub enum KeyType {
    Ed25519,
    Zk,
}

#[derive(Parser, Debug)]
pub struct GenerateKeyArgs {
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

impl GenerateKeyArgs {
    /// Creates arguments programmatically (e.g. from the c-bindings crate).
    /// `auto_approve` skips interactive prompts.
    #[must_use]
    pub const fn new(
        keystore: PathBuf,
        key_type: KeyType,
        key_title: Option<String>,
        auto_approve: bool,
    ) -> Self {
        Self {
            keystore,
            yes: auto_approve,
            key_title,
            key_type,
        }
    }
}

#[derive(Parser, Debug)]
pub struct AddKeyArgs {
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
    zk_key: Option<UnsecuredZkKey>,

    #[clap(
        long = "ed25519",
        value_parser = parse_hex_ed25519_key
    )]
    ed25519_key: Option<UnsecuredEd25519Key>,
}

impl AddKeyArgs {
    /// Creates arguments programmatically (e.g. from the c-bindings crate).
    /// `auto_approve` skips interactive prompts.
    #[must_use]
    pub fn new(
        keystore: PathBuf,
        key_title: Option<String>,
        key: &Key,
        auto_approve: bool,
    ) -> Self {
        let (ed25519_key, zk_key) = match key {
            Key::Ed25519(key) => (Some(key.clone().into_unsecured()), None),
            Key::Zk(key) => (None, Some(key.clone().into_unsecured())),
        };

        Self {
            keystore,
            yes: auto_approve,
            key_title,
            zk_key,
            ed25519_key,
        }
    }

    /// Returns the key provided via the `--ed25519`/`--zk` flags as a [`Key`],
    /// validating that exactly one was given.
    pub fn key(&self) -> Result<Key> {
        match (&self.ed25519_key, &self.zk_key) {
            (Some(ed_secret), None) => Ok(Ed25519Key::from(ed_secret.clone()).into()),
            (None, Some(zk_secret)) => Ok(ZkKey::from(zk_secret.clone()).into()),
            (Some(_), Some(_)) => Err(color_eyre::eyre::eyre!(
                "Please provide either --ed25519 or --zk, not both."
            )),
            (None, None) => Err(color_eyre::eyre::eyre!(
                "You must provide a key via --ed25519 or --zk."
            )),
        }
    }
}

#[derive(Parser, Debug)]
pub struct RemoveKeyArgs {
    /// Path for the keystore file.
    #[clap(long = "keystore", short = 'k', default_value = "keystore.yaml")]
    keystore: PathBuf,

    /// Auto approve interactive promps.
    #[arg(long, short, default_value_t = false)]
    yes: bool,

    #[clap(long = "title")]
    pub key_title: String,
}

impl RemoveKeyArgs {
    /// Creates arguments programmatically (e.g. from the c-bindings crate).
    /// `auto_approve` skips interactive prompts.
    #[must_use]
    pub const fn new(keystore: PathBuf, key_title: String, auto_approve: bool) -> Self {
        Self {
            keystore,
            yes: auto_approve,
            key_title,
        }
    }
}

fn generate_key_into_keystore(
    keystore: &mut Keystore,
    key_title: String,
    key_type: &KeyType,
) -> (KeyId, Key) {
    match key_type {
        KeyType::Ed25519 => {
            let (id, secret_key) = keystore.generate_ed25519(key_title);
            (id, Ed25519Key::from(secret_key).into())
        }
        KeyType::Zk => {
            let (id, secret_key) = keystore.generate_zk(key_title);
            (id, ZkKey::from(secret_key).into())
        }
    }
}

/// Generates a new key, persists it to the keystore and returns the new key's
/// [`KeyId`]. Non-interactive.
pub fn generate_key(args: GenerateKeyArgs) -> Result<KeyId> {
    let GenerateKeyArgs {
        keystore: keystore_path,
        key_title,
        key_type,
        ..
    } = args;

    let mut keystore = Keystore::load(&keystore_path)?;
    let user_key_title = key_title.unwrap_or_else(|| next_user_key_title(&keystore));
    let (key_id, _) = generate_key_into_keystore(&mut keystore, user_key_title, &key_type);
    keystore.save(&keystore_path)?;

    Ok(key_id)
}

pub fn run_generate_key(args: GenerateKeyArgs) -> Result<()> {
    let keystore = Keystore::load(&args.keystore)?;
    let user_key_title = args
        .key_title
        .clone()
        .unwrap_or_else(|| next_user_key_title(&keystore));

    let title_exists = keystore.contains(user_key_title.clone());
    if !overwrite_approved(title_exists, args.yes, || {
        Ok(confirm_overwrite(&format!(
            "A key with the title '{user_key_title}' already exists. Overwrite?"
        ))?)
    })? {
        println!("Action discarded.");
        return Ok(());
    }

    if args.yes || confirm_overwrite("Write key to keystore?")? {
        let key_id = generate_key(args)?;
        println!("KeyID: {key_id}");
        return Ok(());
    }

    // Declined: generate and show the key without persisting it.
    let mut keystore = Keystore::load(&args.keystore)?;
    let user_key_title = args
        .key_title
        .unwrap_or_else(|| next_user_key_title(&keystore));
    let (key_id, key) = generate_key_into_keystore(&mut keystore, user_key_title, &args.key_type);

    println!("KeyID: {key_id}");
    println!("Key: {key:?}");

    Ok(())
}

pub fn run_add_key(args: AddKeyArgs) -> Result<()> {
    let key = args.key()?;

    let AddKeyArgs {
        keystore: keystore_path,
        yes: auto_approve,
        key_title,
        ..
    } = args;

    let mut keystore = Keystore::load(&keystore_path)?;
    let user_key_title = key_title.unwrap_or_else(|| next_user_key_title(&keystore));

    let title_exists = keystore.contains(user_key_title.clone());
    if !overwrite_approved(title_exists, auto_approve, || {
        Ok(confirm_overwrite(&format!(
            "A key with the title '{user_key_title}' already exists. Overwrite?"
        ))?)
    })? {
        println!("Action discarded.");
        return Ok(());
    }

    keystore.set(user_key_title.clone(), key);

    if auto_approve || confirm_overwrite(&format!("Add key '{user_key_title}' to keystore?"))? {
        keystore.save(&keystore_path)?;
        println!("Successfully added key '{user_key_title}' to the keystore.");
    } else {
        println!("Action discarded.");
    }

    Ok(())
}

pub fn run_remove_key(args: RemoveKeyArgs) -> Result<()> {
    let RemoveKeyArgs {
        keystore: keystore_path,
        yes: auto_approve,
        key_title,
    } = args;

    let mut keystore = Keystore::load(&keystore_path)?;
    let title_key = KeyTitle::from(key_title.clone());

    if keystore.get(title_key.clone()).is_none() {
        return Err(KeystoreError::NotFound(title_key).into());
    }

    if auto_approve
        || confirm_overwrite(&format!(
            "Are you sure you want to remove the key '{key_title}'?"
        ))?
    {
        keystore.remove(title_key);
        keystore.save(&keystore_path)?;
        println!("Successfully removed key '{key_title}' from the keystore.");
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

/// Whether a key may be stored under a title: always when no key holds that
/// title, otherwise only with `--yes` or an explicit yes from `confirm`.
fn overwrite_approved(
    title_exists: bool,
    auto_approve: bool,
    confirm: impl FnOnce() -> Result<bool>,
) -> Result<bool> {
    if !title_exists || auto_approve {
        return Ok(true);
    }
    confirm()
}

#[cfg(test)]
mod overwrite_approval_tests {
    use color_eyre::eyre::Result;

    use super::overwrite_approved;

    fn never_asked() -> Result<bool> {
        panic!("asked for confirmation although no key would be overwritten")
    }

    #[test]
    fn a_new_title_is_never_asked_about() {
        assert!(overwrite_approved(false, false, never_asked).unwrap());
        assert!(overwrite_approved(false, true, never_asked).unwrap());
    }

    #[test]
    fn an_existing_title_needs_a_yes_unless_everything_is_approved() {
        assert!(overwrite_approved(true, false, || Ok(true)).unwrap());
        assert!(!overwrite_approved(true, false, || Ok(false)).unwrap());
        assert!(overwrite_approved(true, true, never_asked).unwrap());
    }
}
