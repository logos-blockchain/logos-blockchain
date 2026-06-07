use std::{error::Error, path::PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::run_commands::{
    run_config::run_config,
    run_deposit::run_deposit,
    run_keygen::run_keygen,
    run_withdraw::{
        run_withdraw_combine, run_withdraw_prepare, run_withdraw_sign, run_withdraw_submit,
    },
};

pub(crate) type RunResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Parser, Debug)]
#[command(about = "Terminal UI zone sequencer", version)]
/// Top-level command-line parser for the TUI zone sequencer.
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run_args: NodeKeyArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the interactive inscription TUI.
    Run(NodeKeyArgs),
    /// Update zone channel configuration.
    Config(ConfigArgs),
    /// Build and optionally submit a zone deposit.
    Deposit(DepositArgs),
    /// Create or inspect a local sequencer signing key.
    Keygen(KeygenArgs),
    /// Prepare, sign, combine, or submit zone withdrawals.
    Withdraw {
        #[command(subcommand)]
        command: WithdrawCommand,
    },
}

#[derive(Parser, Debug)]
#[command(about = "Terminal UI zone sequencer - publish text inscriptions")]
/// Shared node endpoint and channel signing key arguments.
pub struct NodeKeyArgs {
    /// Logos blockchain node HTTP endpoint
    #[arg(long, default_value = "http://localhost:8080", env = "NODE_URL")]
    pub node_url: String,

    /// Zone channel ID to use instead of deriving one from the signing key.
    #[arg(long, env = "CHANNEL_ID")]
    pub channel_id: Option<String>,

    /// Path to the signing key file (created if it doesn't exist)
    #[arg(long, default_value = "sequencer.key", env = "KEY_PATH")]
    pub key_path: String,
}

#[derive(Args, Debug)]
/// Arguments for building and optionally submitting a zone deposit.
pub struct DepositArgs {
    #[command(flatten)]
    /// Node endpoint and channel signing key used by the sequencer.
    pub node_key: NodeKeyArgs,
    #[arg(long)]
    /// Path to a cucumber `EXPORT_FUNDS` wallet funds JSON file.
    pub funds: PathBuf,
    #[arg(long)]
    /// Amount to deposit into the zone channel.
    pub amount: u64,
    #[arg(long)]
    /// Deposit metadata stored in the channel deposit op.
    pub metadata: String,
    #[arg(long)]
    /// Inscription message paired with the deposit transaction.
    pub message: String,
    #[arg(long)]
    /// Submit the signed transaction to the node after building it.
    pub submit: bool,
    #[arg(long)]
    /// Wait for finality instead of returning once the tx is observed on chain.
    pub wait_finalized: bool,
    #[arg(long)]
    /// Optional path where a deposit submission summary JSON is written.
    pub out: Option<PathBuf>,
}

#[derive(Args, Debug)]
/// Arguments for updating zone channel configuration.
pub struct ConfigArgs {
    #[command(flatten)]
    /// Node endpoint and channel admin signing key.
    pub node_key: NodeKeyArgs,
    #[arg(long = "authorized-key-path", required = true)]
    /// Paths to signing keys that should be accredited for the channel.
    pub authorized_key_paths: Vec<String>,
    #[arg(long, default_value_t = 1)]
    /// Number of accredited signatures required for future config updates.
    pub configuration_threshold: u16,
    #[arg(long)]
    /// Number of accredited signatures required for withdrawals.
    pub withdraw_threshold: u16,
    #[arg(long, default_value_t = 0)]
    /// Number of slots assigned to an accredited poster.
    pub posting_timeframe: u32,
    #[arg(long, default_value_t = 0)]
    /// Number of slots after which a poster is considered timed out.
    pub posting_timeout: u32,
    #[arg(long)]
    /// Wait for finality instead of returning once the tx is observed on chain.
    pub wait_finalized: bool,
}

#[derive(Args, Debug)]
/// Arguments for creating or inspecting a local sequencer signing key.
pub struct KeygenArgs {
    #[arg(long, default_value = "sequencer.key", env = "KEY_PATH")]
    /// Path to the signing key file to create or inspect.
    pub key_path: String,
}

#[derive(Subcommand, Debug)]
enum WithdrawCommand {
    /// Prepare an unsigned withdrawal intent file.
    Prepare(WithdrawPrepareArgs),
    /// Sign a withdrawal intent with one authorized key.
    Sign(WithdrawSignArgs),
    /// Combine withdrawal signature files into a signed transaction file.
    Combine(WithdrawCombineArgs),
    /// Submit a signed withdrawal transaction file.
    Submit(WithdrawSubmitArgs),
}

#[derive(Args, Debug)]
/// Arguments for preparing an unsigned withdrawal intent file.
pub struct WithdrawPrepareArgs {
    #[command(flatten)]
    /// Node endpoint and channel signing key used to prepare the intent.
    pub node_key: NodeKeyArgs,
    #[arg(long)]
    /// Amount to withdraw from the zone channel.
    pub amount: u64,
    #[arg(long)]
    /// Path to a recipient cucumber `EXPORT_FUNDS` JSON file.
    pub recipient_funds: PathBuf,
    #[arg(long)]
    /// Inscription message paired with the withdrawal transaction.
    pub message: String,
    #[arg(long)]
    /// Path where the withdrawal intent JSON is written.
    pub out: PathBuf,
}

#[derive(Args, Debug)]
/// Arguments for signing a withdrawal intent with one authorized key.
pub struct WithdrawSignArgs {
    #[arg(long)]
    /// Path to the signer key file.
    pub key_path: String,
    #[arg(long = "in")]
    /// Path to the withdrawal intent JSON file.
    pub input: PathBuf,
    #[arg(long)]
    /// Path where the signature JSON is written.
    pub out: PathBuf,
}

#[derive(Args, Debug)]
/// Arguments for combining withdrawal signature files into a signed tx file.
pub struct WithdrawCombineArgs {
    #[arg(long = "in")]
    /// Path to the withdrawal intent JSON file.
    pub input: PathBuf,
    #[arg(long)]
    /// Signature JSON file paths to include.
    pub sig: Vec<PathBuf>,
    #[arg(long)]
    /// Path where the signed withdrawal transaction JSON is written.
    pub out: PathBuf,
}

#[derive(Args, Debug)]
/// Arguments for submitting a signed withdrawal transaction file.
pub struct WithdrawSubmitArgs {
    #[command(flatten)]
    /// Node endpoint and channel signing key used to submit the transaction.
    pub node_key: NodeKeyArgs,
    #[arg(long = "in")]
    /// Path to the signed withdrawal transaction JSON file.
    pub input: PathBuf,
    #[arg(long)]
    /// Wait for finality instead of returning once the tx is observed on chain.
    pub wait_finalized: bool,
}

/// Dispatch a parsed CLI command to the matching command runner.
pub async fn run_cli(cli: Cli) -> RunResult<()> {
    match cli.command {
        Some(Command::Run(args)) => {
            crate::run_commands::run_inscribe::run_inscribe(args).await;
            Ok(())
        }
        Some(Command::Config(args)) => run_config(args).await,
        Some(Command::Deposit(args)) => run_deposit(args).await,
        Some(Command::Keygen(args)) => {
            run_keygen(&args);
            Ok(())
        }
        Some(Command::Withdraw { command }) => match command {
            WithdrawCommand::Prepare(args) => run_withdraw_prepare(args).await,
            WithdrawCommand::Sign(args) => run_withdraw_sign(&args),
            WithdrawCommand::Combine(args) => run_withdraw_combine(args),
            WithdrawCommand::Submit(args) => run_withdraw_submit(args).await,
        },
        None => {
            crate::run_commands::run_inscribe::run_inscribe(cli.run_args).await;
            Ok(())
        }
    }
}
