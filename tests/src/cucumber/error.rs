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
    #[error("Step fail: {message}")]
    StepFail { message: String },
    #[error(transparent)]
    ParseError(#[from] strum::ParseError),
    #[error(transparent)]
    ReqwestError(#[from] reqwest::Error),
}

pub type StepResult = Result<(), StepError>;
