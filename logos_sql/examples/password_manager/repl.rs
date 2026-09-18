//! Interactive terminal adapter for the password manager.
//!
//! Clap owns command parsing and help text. This module maps parsed commands to
//! [`PasswordManager`] operations while leaving SQL and `λSQL` concerns in the
//! domain module.

use std::{
    io::{self, Write as _},
    iter::once,
};

use clap::{Parser, Subcommand};
use logos_sql::{Error as LogosSqlError, TxId};
use tokio::task::spawn_blocking;

use crate::{
    AppError, AppResult,
    passwords::{Credential, CredentialSummary, DisplacementForReview, PasswordManager},
};

#[derive(Debug, Parser)]
#[command(name = "password-manager")]
struct Input {
    #[command(subcommand)]
    command: Command,
}

/// A command accepted by the running password manager.
#[derive(Debug, Subcommand)]
enum Command {
    /// Adds a credential.
    Add {
        label: String,
        account: String,
        #[arg(required = true, num_args = 1..)]
        password: Vec<String>,
    },
    /// Replaces the password stored under a label.
    Update {
        label: String,
        #[arg(required = true, num_args = 1..)]
        password: Vec<String>,
    },
    /// Updates notes independently of the password.
    Notes {
        label: String,
        #[arg(required = true, num_args = 1..)]
        notes: Vec<String>,
    },
    /// Lists displaced writes awaiting review.
    Displacements,
    /// Retries a reviewed write using its original SQL conditions.
    Retry { tx_id: String },
    /// Marks a displacement handled after reviewing it. Does not retry the
    /// write.
    Handle { tx_id: String },
    /// Shows one credential from the local database.
    Show { label: String },
    /// Removes one credential.
    Remove { label: String },
    /// Lists credential labels and accounts.
    List,
    /// Stops the application.
    #[command(alias = "quit")]
    Exit,
}

impl Command {
    /// Parses one line entered at the password-manager prompt.
    fn parse(input: &str) -> Result<Self, clap::Error> {
        let args = once("password-manager").chain(input.split_whitespace());
        Input::try_parse_from(args).map(|input| input.command)
    }
}

/// Reads commands from the terminal until the user exits or input closes.
pub async fn run(manager: &PasswordManager) -> AppResult<()> {
    println!("Password manager is running. Enter `help` to list commands.");
    println!("WARNING: passwords are replicated in plaintext; do not enter real credentials.");

    let mut reported = Vec::new();

    loop {
        let Some(input) = read_input().await? else {
            break;
        };

        if input.trim().is_empty() {
            continue;
        }

        let command = match Command::parse(&input) {
            Ok(command) => command,
            Err(error) => {
                error.print()?;
                continue;
            }
        };

        if matches!(command, Command::Exit) {
            break;
        }

        match handle_command(manager, command, &mut reported).await {
            Ok(Some(tx_id)) => println!("committed locally as {tx_id}; not final"),
            Ok(None) => {}
            Err(error @ AppError::LogosSql(LogosSqlError::UnhandledDisplacements)) => {
                eprintln!("error: {error}");

                println!(
                    "Use `displacements` to list displacements, then `show` or `list` to inspect the data."
                );
                println!(
                    "After review, use `retry <tx-id>` to try the original write or `handle <tx-id>` to leave it alone."
                );
            }
            Err(error) => eprintln!("error: {error}"),
        }
    }

    Ok(())
}

async fn handle_command(
    manager: &PasswordManager,
    command: Command,
    reported: &mut Vec<DisplacementForReview>,
) -> AppResult<Option<TxId>> {
    let tx_id = match command {
        Command::Add {
            label,
            account,
            password,
        } => manager.add(label, account, password.join(" ")).await?,
        Command::Update { label, password } => {
            manager.update_password(label, password.join(" ")).await?
        }
        Command::Notes { label, notes } => manager.update_notes(label, notes.join(" ")).await?,
        Command::Displacements => {
            let displacements = manager.displacements().await?;

            if displacements.is_empty() {
                println!("no displacements awaiting review");
            } else {
                for write in &displacements {
                    println!("{}", write.description);
                }
            }

            *reported = displacements;

            return Ok(None);
        }
        Command::Retry { tx_id } => {
            let index = reported
                .iter()
                .position(|write| write.displacement.tx_id.to_string() == tx_id)
                .ok_or(AppError::DisplacementNotListed)?;
            let displacement = reported[index].displacement.clone();

            let retry_id = manager.retry_displacement(&displacement).await?;
            println!("retry committed locally as {retry_id}; not final");
            println!("The original SQL conditions still apply. Use `show` to check the data.");

            reported.remove(index);

            return Ok(None);
        }
        Command::Handle { tx_id } => {
            let index = reported
                .iter()
                .position(|write| write.displacement.tx_id.to_string() == tx_id)
                .ok_or(AppError::DisplacementNotListed)?;

            manager
                .handle_displacement(reported[index].displacement.clone())
                .await?;
            reported.remove(index);

            println!("marked handled; the write can still return after a reorg");

            return Ok(None);
        }
        Command::Show { label } => {
            print_credential(manager.credential(&label)?);
            return Ok(None);
        }
        Command::Remove { label } => manager.remove(label).await?,
        Command::List => {
            print_credentials(manager.credentials()?);
            return Ok(None);
        }
        Command::Exit => return Ok(None),
    };

    Ok(Some(tx_id))
}

/// Reads terminal input without blocking the runtime that drives `λSQL`.
async fn read_input() -> AppResult<Option<String>> {
    let input = spawn_blocking(|| -> io::Result<Option<String>> {
        print!("password-manager> ");
        io::stdout().flush()?;

        let mut input = String::new();
        let bytes_read = io::stdin().read_line(&mut input)?;

        Ok((bytes_read > 0).then_some(input))
    })
    .await??;

    Ok(input)
}

fn print_credential(credential: Option<Credential>) {
    let Some(credential) = credential else {
        println!("credential not found");
        return;
    };

    println!("{}", credential.label);
    println!("  account: {}", credential.account);
    println!("  password: {}", credential.password);
    println!("  notes: {}", credential.notes);
}

fn print_credentials(credentials: Vec<CredentialSummary>) {
    for credential in credentials {
        println!("{} ({})", credential.label, credential.account);
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn handling_a_displacement_is_separate_from_editing_a_password() {
        assert!(
            matches!(Command::parse("handle abc123").unwrap(), Command::Handle { tx_id } if tx_id == "abc123")
        );
        assert!(Command::parse("handle").is_err());
        assert!(
            matches!(Command::parse("update email fake password").unwrap(), Command::Update { label, password } if label == "email" && password == ["fake", "password"])
        );
    }

    #[test]
    fn retry_uses_a_transaction_id_not_a_new_password() {
        assert!(
            matches!(Command::parse("retry abc123").unwrap(), Command::Retry { tx_id } if tx_id == "abc123")
        );

        assert!(Command::parse("retry").is_err());
        assert!(Command::parse("retry abc123 new-password").is_err());
    }
}
