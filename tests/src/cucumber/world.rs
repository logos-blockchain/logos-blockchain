use std::{env, path::PathBuf, time::Duration};

use cucumber::World;
use testing_framework_core::scenario::{
    Builder, NodeControlCapability, Scenario, ScenarioBuildError, ScenarioBuilder,
};
use testing_framework_workflows::{ScenarioBuilderExt as _, expectations::ConsensusLiveness};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeployerKind {
    #[default]
    Local,
    Compose,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkKind {
    Star,
}

#[derive(Debug, Default, Clone)]
pub struct RunState {
    pub result: Option<Result<(), String>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScenarioSpec {
    pub topology: Option<TopologySpec>,
    pub duration_secs: Option<u64>,
    pub wallets: Option<WalletSpec>,
    pub transactions: Option<TransactionSpec>,
    pub consensus_liveness: Option<ConsensusLivenessSpec>,
}

#[derive(Debug, Clone, Copy)]
pub struct TopologySpec {
    pub validators: usize,
    pub network: NetworkKind,
}

#[derive(Debug, Clone, Copy)]
pub struct WalletSpec {
    pub total_funds: u64,
    pub users: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct TransactionSpec {
    pub rate_per_block: u64,
    pub users: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsensusLivenessSpec {
    pub lag_allowance: Option<u64>,
}

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
}

pub type StepResult = Result<(), StepError>;

#[derive(World, Debug, Default)]
pub struct CucumberWorld {
    pub deployer: Option<DeployerKind>,
    pub spec: ScenarioSpec,
    pub run: RunState,
    pub membership_check: bool,
    pub readiness_checks: bool,
}

impl CucumberWorld {
    pub const fn set_deployer(&mut self, kind: DeployerKind) -> StepResult {
        self.deployer = Some(kind);
        Ok(())
    }

    pub fn set_topology(&mut self, validators: usize, network: NetworkKind) -> StepResult {
        self.spec.topology = Some(TopologySpec {
            validators: positive_usize("validators", validators)?,
            network,
        });
        Ok(())
    }

    pub fn set_run_duration(&mut self, seconds: u64) -> StepResult {
        self.spec.duration_secs = Some(positive_u64("duration", seconds)?);
        Ok(())
    }

    pub fn set_wallets(&mut self, total_funds: u64, users: usize) -> StepResult {
        self.spec.wallets = Some(WalletSpec {
            total_funds,
            users: positive_usize("wallet users", users)?,
        });
        Ok(())
    }

    pub fn set_transactions_rate(
        &mut self,
        rate_per_block: u64,
        users: Option<usize>,
    ) -> StepResult {
        if self.spec.transactions.is_some() {
            return Err(StepError::InvalidArgument {
                message: "transactions workload already configured".to_owned(),
            });
        }

        if users.is_some_and(|u| u == 0) {
            return Err(StepError::InvalidArgument {
                message: "transactions users must be > 0".to_owned(),
            });
        }

        self.spec.transactions = Some(TransactionSpec {
            rate_per_block: positive_u64("transactions rate", rate_per_block)?,
            users,
        });
        Ok(())
    }

    pub const fn enable_consensus_liveness(&mut self) -> StepResult {
        if self.spec.consensus_liveness.is_none() {
            self.spec.consensus_liveness = Some(ConsensusLivenessSpec {
                lag_allowance: None,
            });
        }

        Ok(())
    }

    pub fn set_consensus_liveness_lag_allowance(&mut self, blocks: u64) -> StepResult {
        let blocks = positive_u64("lag allowance", blocks)?;

        self.spec.consensus_liveness = Some(ConsensusLivenessSpec {
            lag_allowance: Some(blocks),
        });

        Ok(())
    }

    pub fn build_local_scenario(&self) -> Result<Scenario<()>, StepError> {
        self.preflight(DeployerKind::Local)?;
        let builder = self.make_builder_for_deployer::<()>(DeployerKind::Local)?;
        builder
            .build()
            .map_err(|source| StepError::ScenarioBuild { source })
    }

    pub fn build_compose_scenario(&self) -> Result<Scenario<NodeControlCapability>, StepError> {
        self.preflight(DeployerKind::Compose)?;
        let builder =
            self.make_builder_for_deployer::<NodeControlCapability>(DeployerKind::Compose)?;
        builder
            .build()
            .map_err(|source| StepError::ScenarioBuild { source })
    }

    pub fn preflight(&self, expected: DeployerKind) -> Result<(), StepError> {
        let actual = self.deployer.ok_or(StepError::MissingDeployer)?;
        if actual != expected {
            return Err(StepError::DeployerMismatch { expected, actual });
        }

        if !is_truthy_env("POL_PROOF_DEV_MODE") {
            return Err(StepError::Preflight {
                message:
                    "POL_PROOF_DEV_MODE must be set to \"true\" (or \"1\") for practical test runs."
                        .to_owned(),
            });
        }

        if expected == DeployerKind::Local {
            let node_ok = env::var_os("NOMOS_NODE_BIN")
                .map(PathBuf::from)
                .is_some_and(|p| p.is_file())
                || shared_host_bin_path("nomos-node").is_file();

            if !(node_ok) {
                return Err(StepError::Preflight {
                    message: "Missing Logos host binaries. Set NOMOS_NODE_BIN, or run \
                    `scripts/run/run-examples.sh host` to restore them into \
                    `testing-framework/assets/stack/bin`."
                        .to_owned(),
                });
            }
        }

        Ok(())
    }

    fn make_builder_for_deployer<Caps: Default>(
        &self,
        expected: DeployerKind,
    ) -> Result<Builder<Caps>, StepError> {
        let actual = self.deployer.ok_or(StepError::MissingDeployer)?;
        if actual != expected {
            return Err(StepError::DeployerMismatch { expected, actual });
        }

        let topology = self.spec.topology.ok_or(StepError::MissingTopology)?;
        let duration_secs = self
            .spec
            .duration_secs
            .ok_or(StepError::MissingRunDuration)?;

        let mut builder: Builder<Caps> = make_builder(topology).with_capabilities(Caps::default());

        builder = builder.with_run_duration(Duration::from_secs(duration_secs));

        if let Some(wallets) = self.spec.wallets {
            builder = builder.initialize_wallet(wallets.total_funds, wallets.users);
        }

        if let Some(tx) = self.spec.transactions {
            builder = builder.transactions_with(|flow| {
                let mut flow = flow.rate(tx.rate_per_block);
                if let Some(users) = tx.users {
                    flow = flow.users(users);
                }
                flow
            });
        }

        if let Some(liveness) = self.spec.consensus_liveness {
            if let Some(lag) = liveness.lag_allowance {
                builder =
                    builder.with_expectation(ConsensusLiveness::default().with_lag_allowance(lag));
            } else {
                builder = builder.expect_consensus_liveness();
            }
        }

        Ok(builder)
    }
}

fn make_builder(topology: TopologySpec) -> Builder<()> {
    ScenarioBuilder::topology_with(|t| {
        let base = match topology.network {
            NetworkKind::Star => t.network_star(),
        };
        base.validators(topology.validators)
    })
}

fn is_truthy_env(key: &str) -> bool {
    env::var(key)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn positive_usize(label: &str, value: usize) -> Result<usize, StepError> {
    if value == 0 {
        Err(StepError::InvalidArgument {
            message: format!("{label} must be > 0"),
        })
    } else {
        Ok(value)
    }
}

fn positive_u64(label: &str, value: u64) -> Result<u64, StepError> {
    if value == 0 {
        Err(StepError::InvalidArgument {
            message: format!("{label} must be > 0"),
        })
    } else {
        Ok(value)
    }
}

pub fn parse_deployer(value: &str) -> Result<DeployerKind, StepError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "local" | "host" => Ok(DeployerKind::Local),
        "compose" | "docker" => Ok(DeployerKind::Compose),
        other => Err(StepError::UnsupportedDeployer {
            value: other.to_owned(),
        }),
    }
}

#[must_use]
pub fn shared_host_bin_path(binary_name: &str) -> PathBuf {
    let cucumber_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cucumber_dir.join("../assets/stack/bin").join(binary_name)
}
