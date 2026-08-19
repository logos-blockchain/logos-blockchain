//! A password manager built on `λSQL`.
//!
//! This first version stores and replicates passwords in plaintext. It exists
//! only to demonstrate the `λSQL` application API. Do not enter real
//! credentials. Application-side encryption will be added in a follow-up.
//!
//! Run with `--help` to see startup configuration. Each option also accepts its
//! corresponding `LSQL_*` environment variable. After startup, enter `help` to
//! see the available commands. A short session could look like this:
//!
//! ```text
//! add email andrus@example.org not-a-real-password
//! update email another-fake-password
//! show email
//! list
//! exit
//! ```

mod config;
mod passwords;
mod repl;

use std::error::Error;

use passwords::PasswordManager;

type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> AppResult<()> {
    let manager = PasswordManager::start(config::from_args()).await?;

    let result = repl::run(&manager).await;

    manager.shutdown().await?;

    result
}
