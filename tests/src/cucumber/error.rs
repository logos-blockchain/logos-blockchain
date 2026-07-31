use std::io::Error as IoError;

use hex::FromHexError;
use lb_core::{codec::Error, mantle::transactions::VerificationError};
use lb_testing_framework::configs::wallet::WalletConfigError;
use lb_utils::bounded::BoundedError;
use lb_wallet::WalletError;
use lb_zksign::ZkSignError;
use testing_framework_core::scenario::ScenarioBuildError;
use testing_framework_runner_local::ManualClusterError;
use thiserror::Error;

use crate::cucumber::world::DeployerKind;

#[derive(Debug, Error)]
pub enum StepError {
    #[error("deployer is not selected; set it first (e.g. `Given deployer is \"local\"`)")]
    MissingDeployer,
    #[error("scenario topology is not configured")]
    MissingTopology,
    #[error("Step requires a table argument, but none was provided")]
    MissingTable,
    #[error("scenario run duration is not configured")]
    MissingRunDuration,
    #[error("unsupported deployer kind: {value}")]
    UnsupportedDeployer { value: String },
    #[error("step requires deployer {expected:?}, but current deployer is {actual:?}")]
    DeployerMismatch {
        expected: DeployerKind,
        actual: DeployerKind,
    },
    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },
    #[error("{message}")]
    Preflight { message: String },
    #[error("failed to build scenario: {source}")]
    ScenarioBuild {
        #[source]
        source: ScenarioBuildError,
    },
    #[error("{message}")]
    RunFailed { message: String },
    #[error(transparent)]
    ManualCluster(#[from] ManualClusterError),
    #[error("Logical error: {message}")]
    LogicalError { message: String },
    #[error("Operation timed out: {message}")]
    Timeout { message: String },
    #[error("Step fail: {message}")]
    StepFail { message: String },
    #[error(transparent)]
    ParseError(#[from] strum::ParseError),
    #[error(transparent)]
    ReqwestError(#[from] reqwest::Error),
    #[error(transparent)]
    CommonHttpError(#[from] lb_common_http_client::Error),
    #[error(transparent)]
    WalletConfigError(#[from] WalletConfigError),
    #[error(transparent)]
    WalletError(#[from] WalletError),
    #[error(transparent)]
    ZkSignError(#[from] ZkSignError),
    #[error(transparent)]
    VerificationError(#[from] VerificationError),
    #[error("Step requires a wallet, but none was provided")]
    MissingWallet,
    #[error(transparent)]
    FromHexError(#[from] FromHexError),
    #[error(transparent)]
    Error(#[from] Error),
    #[error(transparent)]
    IoError(#[from] IoError),
    #[error("User configuration error: {0}")]
    UserConfigError(String),
    #[error("Wallet does not have enough funds, available={available}")]
    FundsDeficit {
        available: u64,
        num_utxos_required: usize,
        value_per_utxos_required: u64,
    },
    #[error(
        "fee horizon expired: paid fee {paid_fee}, current required fee {required_fee} (prepared epoch {prepared_at_epoch}, valid through {valid_through_epoch})"
    )]
    FeeHorizonExpired {
        paid_fee: u64,
        required_fee: u64,
        prepared_at_epoch: u32,
        valid_through_epoch: u32,
    },
    #[error(
        "fee horizon expired after preparing wallet '{wallet_name}' ({prepared_count} transaction(s)): current epoch {current_epoch} exceeds valid-through epoch {valid_through_epoch} (prepared epoch {prepared_at_epoch})"
    )]
    FeeHorizonExceededAfterWalletBatch {
        wallet_name: String,
        prepared_count: usize,
        current_epoch: u64,
        prepared_at_epoch: u32,
        valid_through_epoch: u32,
    },
    #[error(
        "fee horizon expired before submission: current epoch {current_epoch} exceeds valid-through epoch {valid_through_epoch} (prepared epoch {prepared_at_epoch})"
    )]
    FeeHorizonExceeded {
        current_epoch: u64,
        prepared_at_epoch: u32,
        valid_through_epoch: u32,
    },
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
}

pub type StepResult = Result<(), StepError>;
