//! Interactive terminal adapter for the password manager.
//!
//! Clap owns command parsing and help text. This module maps parsed commands to
//! [`PasswordManager`] operations while leaving SQL and `λSQL` concerns in the
//! domain module.

use std::io::{self, Write as _};

use clap::{Parser, Subcommand};
use logos_sql::TxId;
use uuid::Uuid;

use crate::{
    AppResult,
    passwords::{Credential, CredentialSummary, PasswordManager},
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
        let args = std::iter::once("password-manager").chain(input.split_whitespace());
        Input::try_parse_from(args).map(|input| input.command)
    }
}

/// Reads commands from the terminal until the user exits or input closes.
pub async fn run(manager: &PasswordManager) -> AppResult<()> {
    println!("Password manager is running. Enter `help` to list commands.");
    println!("WARNING: passwords are replicated in plaintext; do not enter real credentials.");

    while let Some(input) = read_input().await? {
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

        match handle_command(manager, command).await {
            Ok(Some((request_id, tx_id))) => {
                println!("request {request_id} committed locally as {tx_id}");
            }
            Ok(None) => {}
            Err(error) => eprintln!("error: {error}"),
        }
    }

    Ok(())
}

async fn handle_command(
    manager: &PasswordManager,
    command: Command,
) -> AppResult<Option<(Uuid, TxId)>> {
    let request_id = Uuid::new_v4();

    let tx_id = match command {
        Command::Add {
            label,
            account,
            password,
        } => {
            manager
                .add(request_id.to_string(), label, account, password.join(" "))
                .await?
        }
        Command::Update { label, password } => {
            manager
                .update_password(request_id.to_string(), label, password.join(" "))
                .await?
        }
        Command::Show { label } => {
            print_credential(manager.credential(&label)?);
            return Ok(None);
        }
        Command::Remove { label } => manager.remove(request_id.to_string(), label).await?,
        Command::List => {
            print_credentials(manager.credentials()?);
            return Ok(None);
        }
        Command::Exit => return Ok(None),
    };

    Ok(Some((request_id, tx_id)))
}

/// Reads terminal input without blocking the runtime that drives `λSQL`.
async fn read_input() -> AppResult<Option<String>> {
    let input = tokio::task::spawn_blocking(|| -> io::Result<Option<String>> {
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
}

fn print_credentials(credentials: Vec<CredentialSummary>) {
    for credential in credentials {
        println!("{} ({})", credential.label, credential.account);
    }
}
