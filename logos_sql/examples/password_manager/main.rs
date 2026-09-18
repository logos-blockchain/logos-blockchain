//! A password manager built on `λSQL`.
//!
//! This first version stores and replicates passwords in plaintext. It exists
//! only to demonstrate the `λSQL` application API. Do not enter real
//! credentials. Application-side encryption will be added in a follow-up.
//!
//! Run with `--help` to see startup configuration. Each option also accepts its
//! corresponding `LOGOS_SQL_*` environment variable. After startup, enter
//! `help` to see the available commands. A short session could look like this:
//!
//! ```text
//! add email andrus@example.org not-a-real-password
//! update email another-fake-password
//! notes email recovery codes stored separately
//! show email
//! displacements
//! list
//! exit
//! ```
//!
//! # Conflicts
//!
//! Two copies of the app can make changes before seeing each other's writes.
//! A command may succeed locally, then lose its place in the shared history.
//! Logos SQL reports that write as displaced and blocks new writes until the
//! user reviews it. This example does not automatically retry displaced SQL.
//!
//! Use `displacements` to list displacements, then `show <label>` or `list` to
//! inspect the current data. Use `retry <tx-id>` if the original change is
//! still wanted, or `handle <tx-id>` to continue without retrying it. A
//! successful retry marks that displacement handled; a failed retry leaves it
//! available for review.
//!
//! Password edits check the old password, and notes edits check the old notes.
//! Changing notes does not prevent a password retry, but changing the password
//! does. Deletes only remove an entry if its account, password, and notes still
//! match what the app read when preparing the command. This prevents a retried
//! delete from removing an entry someone has since edited.
//!
//! Retrying uses the saved SQL and its original values, without reading new
//! ones for the conditions.
//! If a condition fails, the SQL succeeds without changing a row. Use `show`
//! to inspect the result. Adds use a primary key to reject an occupied label.
//!
//! If another displacement arrives during review, new writes remain blocked
//! until that displacement is handled too. The app explains what to do when
//! a command is blocked.
//!
//! Marking a displacement handled clears it from the review list.
//! It does not change the database or channel history.
//!
//! These checks compare values, not edit
//! history: an old add can succeed again after deletion, and a condition can
//! pass again if a value changes back. This is why retries are a user decision,
//! not a background loop.
//!
//! Use a fresh channel and state directory for this example.

mod config;
mod passwords;
mod repl;

use std::io;

use logos_sql::Error as LogosSqlError;
use passwords::PasswordManager;
use rusqlite::Error as SqliteError;
use tokio::task::JoinError;

type AppResult<T> = Result<T, AppError>;

/// Errors from password-manager commands and terminal input.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    LogosSql(#[from] LogosSqlError),

    #[error(transparent)]
    Sqlite(#[from] SqliteError),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("terminal input stopped: {0}")]
    InputStopped(#[from] JoinError),

    #[error("credential not found")]
    CredentialNotFound,

    #[error("displacement not listed; run `displacements` to review it first")]
    DisplacementNotListed,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> AppResult<()> {
    let manager = PasswordManager::start(config::from_args()).await?;

    let result = repl::run(&manager).await;

    manager.shutdown().await?;

    result
}
