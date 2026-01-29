pub mod validator;

use std::{path::PathBuf, sync::LazyLock};

use reqwest::Client;
use tempfile::TempDir;
pub use validator::{Pool, Validator, create_validator_config};

const LOGS_PREFIX: &str = "__logs";
static CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

fn create_tempdir() -> std::io::Result<TempDir> {
    // It's easier to use the current location instead of OS-default tempfile
    // location because Github Actions can easily access files in the current
    // location using wildcard to upload them as artifacts.
    TempDir::new_in(std::env::current_dir()?)
}

fn persist_tempdir(tempdir: &mut TempDir, label: &str) -> std::io::Result<()> {
    println!(
        "{}: persisting directory at {}",
        label,
        tempdir.path().display()
    );
    // we need ownership of the dir to persist it
    let dir = std::mem::replace(tempdir, tempfile::tempdir()?);
    drop(dir.keep());
    Ok(())
}

#[must_use]
pub fn get_exe_path(profile_name: &str) -> PathBuf {
    let binary_path = std::env::current_dir()
        .unwrap()
        .join("../")
        .join("target")
        .join(profile_name)
        .join("logos-blockchain-node");

    if std::fs::exists(&binary_path).unwrap() {
        binary_path
    } else {
        panic!(
            "\nCould not find logos-blockchain binary for profile '{}'\n",
            binary_path.display()
        );
    }
}
