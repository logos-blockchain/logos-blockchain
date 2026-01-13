pub mod executor;
pub mod validator;

use std::{path::PathBuf, sync::LazyLock};

use reqwest::Client;
use tempfile::TempDir;

const DA_GET_TESTING_ENDPOINT_ERROR: &str = "Failed to connect to testing endpoint. The binary was likely built without the 'testing' feature. Try: cargo build --locked --all-features";

const LOGS_PREFIX: &str = "__logs";
static CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

const USE_DEBUG_BINARIES: &str = "USE_DEBUG_BINARIES";
const USE_RELEASE_BINARIES: &str = "USE_RELEASE_BINARIES";

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
pub fn get_exe_path(debug_binary: &str, release_binary: &str) -> PathBuf {
    let debug_binary = std::env::current_dir().unwrap().join(debug_binary);
    let release_binary = std::env::current_dir().unwrap().join(release_binary);
    match (
        std::env::var(USE_DEBUG_BINARIES).is_ok(),
        std::env::var(USE_RELEASE_BINARIES).is_ok(),
    ) {
        (true, false) => {
            if std::fs::exists(&debug_binary).unwrap() {
                debug_binary
            } else {
                panic!(
                    "\nCould not find nomos binary in debug path '{}'\n",
                    debug_binary.display()
                );
            }
        }
        (false, true) => {
            if std::fs::exists(&release_binary).unwrap() {
                release_binary
            } else {
                panic!(
                    "\nCould not find nomos binary in release path '{}'\n",
                    release_binary.display()
                );
            }
        }
        (false, false) => {
            if std::fs::exists(&debug_binary).unwrap() {
                debug_binary
            } else if std::fs::exists(&release_binary).unwrap() {
                release_binary
            } else {
                panic!(
                    "\nCould not find nomos binary in debug '{}' or release path '{}'\n",
                    debug_binary.display(),
                    release_binary.display()
                );
            }
        }
        (true, true) => {
            panic!(
                "\nOnly one of 'USE_DEBUG_BINARIES' or 'USE_RELEASE_BINARIES' environment variables \
                can be set.\n",
            );
        }
    }
}
