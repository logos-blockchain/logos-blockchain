use std::{error::Error, path::PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::run_commands::{
    run_balance::{run_state_balance, run_state_full},
    run_config::{
        run_config, run_config_combine, run_config_prepare, run_config_sign, run_config_submit,
    },
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
    /// Apply, prepare, sign, combine, or submit zone channel configuration
    /// updates.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Print zone channel state.
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
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

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Apply a single-signer channel configuration update.
    Apply(ConfigArgs),
    /// Prepare an unsigned channel configuration intent file.
    Prepare(ConfigPrepareArgs),
    /// Sign a channel configuration intent with one authorized key.
    Sign(ConfigSignArgs),
    /// Combine configuration signature files into a signed transaction file.
    Combine(ConfigCombineArgs),
    /// Submit a signed channel configuration transaction file.
    Submit(ConfigSubmitArgs),
}

#[derive(Subcommand, Debug)]
enum StateCommand {
    /// Print the channel balance and configuration state.
    Full(StateArgs),
    /// Print the channel balance.
    Balance(StateArgs),
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

    /// Node wallet public key (hex, 32 bytes) used to pay transaction fees.
    /// The value comes from the node's own configuration. When absent,
    /// transactions are built fee-less (only valid while gas prices are
    /// zero).
    #[arg(long, env = "FUNDING_PK")]
    pub funding_pk: Option<String>,

    /// Cap on a single transaction's fee (in gas units) when funding via
    /// `--funding-pk`.
    #[arg(long, default_value_t = 1_000_000, env = "MAX_TX_FEE")]
    pub max_tx_fee: u64,
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
    /// Wait for finality instead of returning once the tx is observed on chain.
    pub wait_finalized: bool,
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
/// Arguments for printing zone channel state.
pub struct StateArgs {
    #[command(flatten)]
    /// Node endpoint and channel key used to resolve the channel.
    pub node_key: NodeKeyArgs,
}

#[derive(Args, Debug)]
/// Arguments for preparing an unsigned channel configuration intent file.
pub struct ConfigPrepareArgs {
    #[command(flatten)]
    /// Node endpoint and channel signing key.
    pub node_key: NodeKeyArgs,
    #[arg(long = "authorized-key-path", required = true)]
    /// Paths to signing keys that should be accredited for the channel.
    pub authorized_key_paths: Vec<String>,
    #[arg(long)]
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
    /// Path where the configuration intent JSON is written.
    pub out: PathBuf,
}

#[derive(Args, Debug)]
/// Arguments for signing a channel configuration intent with one key.
pub struct ConfigSignArgs {
    #[arg(long)]
    /// Path to the signer key file.
    pub key_path: String,
    #[arg(long = "in")]
    /// Path to the configuration intent JSON file.
    pub input: PathBuf,
    #[arg(long)]
    /// Path where the signature JSON is written.
    pub out: PathBuf,
}

#[derive(Args, Debug)]
/// Arguments for combining configuration signature files into a signed tx file.
pub struct ConfigCombineArgs {
    #[arg(long = "in")]
    /// Path to the configuration intent JSON file.
    pub input: PathBuf,
    #[arg(long)]
    /// Signature JSON file paths to include.
    pub sig: Vec<PathBuf>,
    #[arg(long)]
    /// Path where the signed configuration transaction JSON is written.
    pub out: PathBuf,
}

#[derive(Args, Debug)]
/// Arguments for submitting a signed channel configuration transaction file.
pub struct ConfigSubmitArgs {
    #[command(flatten)]
    /// Node endpoint and channel signing key used to submit the transaction.
    pub node_key: NodeKeyArgs,
    #[arg(long = "in")]
    /// Path to the signed configuration transaction JSON file.
    pub input: PathBuf,
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
        Some(Command::Config { command }) => match command {
            ConfigCommand::Apply(args) => run_config(args).await,
            ConfigCommand::Prepare(args) => run_config_prepare(args).await,
            ConfigCommand::Sign(args) => run_config_sign(&args),
            ConfigCommand::Combine(args) => run_config_combine(args),
            ConfigCommand::Submit(args) => run_config_submit(args).await,
        },
        Some(Command::State { command }) => match command {
            StateCommand::Full(args) => run_state_full(args).await,
            StateCommand::Balance(args) => run_state_balance(args).await,
        },
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
