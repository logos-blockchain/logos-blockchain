use std::{
    collections::HashMap,
    env,
    num::NonZero,
    path::{Path, PathBuf},
    time::Duration,
};

use cucumber::World;
use derivative::Derivative;
use lb_node::config::RunConfig;
use lb_testing_framework::{
    LbcEnv, LbcManualCluster, ScenarioBuilder, ScenarioBuilderExt as _, workloads,
};
use testing_framework_core::scenario::{NodeControlCapability, Scenario, StartedNode};
use tracing::warn;

use crate::{
    BIN_PATH_DEBUG, BIN_PATH_RELEASE,
    cucumber::{
        TARGET,
        defaults::{LOGOS_BLOCKCHAIN_NODE_BIN, init_node_log_dir_defaults, set_default_env},
        error::{StepError, StepResult},
        utils::{make_builder, shared_host_bin_path},
    },
    non_zero,
};

type ScenarioBuilderWith = ScenarioBuilder;
type ConsensusLiveness = workloads::ConsensusLiveness;

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

#[derive(Debug, Default, Clone)]
pub struct ScenarioSpec {
    pub topology: Option<TopologySpec>,
    pub duration_secs: Option<NonZero<u64>>,
    pub wallets: Option<WalletSpec>,
    pub transactions: Option<TransactionSpec>,
    pub consensus_liveness: Option<ConsensusLivenessSpec>,
}

#[derive(Debug, Clone)]
pub struct TopologySpec {
    pub nodes: NonZero<usize>,
    pub network: NetworkKind,
    pub scenario_base_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub struct WalletSpec {
    pub total_funds: u64,
    pub users: NonZero<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct TransactionSpec {
    pub rate_per_block: NonZero<u64>,
    pub users: Option<NonZero<usize>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsensusLivenessSpec {
    pub lag_allowance: Option<NonZero<u64>>,
}

#[derive(World, Derivative)]
#[derivative(Debug, Default)]
pub struct CucumberWorld {
    pub deployer: Option<DeployerKind>,
    pub spec: ScenarioSpec,
    pub run: RunState,
    pub membership_check: bool,
    pub readiness_checks: bool,
    #[derivative(Debug = "ignore")]
    #[derivative(Default(value = "None"))]
    pub local_cluster: Option<LbcManualCluster>,
    #[derivative(Debug = "ignore")]
    pub nodes_info: HashMap<String, NodeInfo>,
    pub scenario_base_dir: PathBuf,
}

pub type ChainInfoMap = HashMap<u64, String>;

/// Information about a started node in the world
pub struct NodeInfo {
    /// Node name
    pub name: String,
    pub started_node: StartedNode<LbcEnv>,
    /// General node configuration used to start the node
    pub run_config: Option<RunConfig>,
    /// Chain height vs. hash at that height
    pub chain_info: ChainInfoMap,
}

impl NodeInfo {
    /// Convenience: record a node's current tip at its current height.
    pub fn upsert_tip(&mut self, height: u64, tip_hash_hex: String) {
        self.chain_info.insert(height, tip_hash_hex);
    }

    /// Returns the highest height for which we have a cached hash (if any).
    #[must_use]
    pub fn best_height(&self) -> Option<u64> {
        self.chain_info.keys().copied().max()
    }

    /// Returns a reference to the full map of cached height -> hash.
    #[must_use]
    pub const fn chain_info(&self) -> &ChainInfoMap {
        &self.chain_info
    }
}

impl CucumberWorld {
    /// Get the best known height for the given node, if any. This is based on
    /// the cached height -> hash information stored in the world for each
    /// node.
    pub fn node_best_height(&self, node_name: &String) -> Result<Option<u64>, StepError> {
        let node = self
            .nodes_info
            .get(node_name)
            .ok_or(StepError::LogicalError {
                message: format!("Runtime node '{node_name}' not found"),
            })?;
        Ok(node.best_height())
    }

    /// Set the deployer kind for this scenario.
    pub const fn set_deployer(&mut self, deployer: DeployerKind) {
        self.deployer = Some(deployer);
    }

    /// Set the directory where scenario artifacts should be stored.
    pub fn set_scenario_base_dir(&mut self, log_dir: &Path, deployer: &DeployerKind) {
        let log_dir = PathBuf::from(log_dir);
        init_node_log_dir_defaults(deployer, Some(&log_dir));
        self.scenario_base_dir.clone_from(&log_dir);
        if let Some(topology) = self.spec.topology.as_mut() {
            topology.scenario_base_dir = log_dir;
        }
    }

    /// Configure the scenario topology (number of nodes and network layout).
    pub fn set_topology(&mut self, nodes: usize, network: NetworkKind) -> StepResult {
        self.spec.topology = Some(TopologySpec {
            nodes: non_zero!("nodes", nodes)?,
            network,
            scenario_base_dir: self.scenario_base_dir.clone(),
        });
        Ok(())
    }

    /// Configure the scenario run duration in seconds.
    pub fn set_run_duration(&mut self, seconds: u64) -> StepResult {
        self.spec.duration_secs = Some(non_zero!("duration", seconds)?);
        Ok(())
    }

    // Configure the scenario wallets with total funds and number of users.
    pub fn set_wallets(&mut self, total_funds: u64, users: usize) -> StepResult {
        self.spec.wallets = Some(WalletSpec {
            total_funds,
            users: non_zero!("wallet users", users)?,
        });
        Ok(())
    }

    /// Configure the scenario transactions with a rate per block and optional
    /// number of users.
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

        self.spec.transactions = Some(TransactionSpec {
            rate_per_block: non_zero!("transactions rate", rate_per_block)?,
            users: match users {
                Some(val) => Some(non_zero!("transactions users", val)?),
                None => None,
            },
        });
        Ok(())
    }

    /// Enable the consensus liveness expectation for this scenario.
    pub const fn enable_consensus_liveness(&mut self) {
        if self.spec.consensus_liveness.is_none() {
            self.spec.consensus_liveness = Some(ConsensusLivenessSpec {
                lag_allowance: None,
            });
        }
    }

    /// Set the consensus liveness lag allowance in blocks. This configures how
    /// far behind the target height the nodes are allowed to be while still
    /// satisfying the expectation.
    pub fn set_consensus_liveness_lag_allowance(&mut self, blocks: u64) -> StepResult {
        self.spec.consensus_liveness = Some(ConsensusLivenessSpec {
            lag_allowance: Some(non_zero!("lag allowance", blocks)?),
        });

        Ok(())
    }

    /// Build a scenario for local deployment based on the current world
    /// configuration. This performs necessary preflight checks and returns
    /// a built scenario ready for deployment.
    pub fn build_local_scenario(&self) -> Result<Scenario<LbcEnv>, StepError> {
        let builder = self.make_builder_for_deployer(DeployerKind::Local)?;
        builder
            .build()
            .map_err(|source| StepError::ScenarioBuild { source })
    }

    /// Build a scenario for compose deployment based on the current world
    /// configuration. This performs necessary preflight checks and returns
    /// a built scenario ready for deployment.
    pub fn build_compose_scenario(
        &self,
    ) -> Result<Scenario<LbcEnv, NodeControlCapability>, StepError> {
        let builder = self.make_builder_for_deployer(DeployerKind::Compose)?;
        builder
            .enable_node_control()
            .build()
            .map_err(|source| StepError::ScenarioBuild { source })
    }

    /// Perform preflight checks to ensure the world is properly configured for
    /// the expected deployer kind.
    pub fn preflight(&self, expected: DeployerKind) -> Result<(), StepError> {
        let actual = self.deployer.ok_or(StepError::MissingDeployer)?;
        if actual != expected {
            return Err(StepError::DeployerMismatch { expected, actual });
        }

        if expected == DeployerKind::Local {
            let node_ok = env::var_os(LOGOS_BLOCKCHAIN_NODE_BIN)
                .map(PathBuf::from)
                .is_some_and(|p| p.is_file())
                || shared_host_bin_path("logos-blockchain-node").is_file();

            if !(node_ok) {
                if let Some(default_exe_path) = {
                    env::current_dir().map_or(None, |current_dir| {
                        let debug_binary = current_dir.join(BIN_PATH_DEBUG);
                        let release_binary = current_dir.join(BIN_PATH_RELEASE);
                        if matches!(std::fs::exists(&debug_binary), Ok(true)) {
                            Some(debug_binary)
                        } else if matches!(std::fs::exists(&release_binary), Ok(true)) {
                            Some(release_binary)
                        } else {
                            None
                        }
                    })
                } {
                    if env::var_os(LOGOS_BLOCKCHAIN_NODE_BIN).is_some() {
                        warn!(
                            target: TARGET,
                            "'{LOGOS_BLOCKCHAIN_NODE_BIN:?}' does not point to a valid file, \
                            Overriding '{LOGOS_BLOCKCHAIN_NODE_BIN}' to point to '{}'.",
                            default_exe_path.display()
                        );
                    }
                    set_default_env(
                        LOGOS_BLOCKCHAIN_NODE_BIN,
                        &default_exe_path.display().to_string(),
                    );
                    return Ok(());
                }

                return Err(StepError::Preflight {
                    message: format!(
                        "Missing Logos host binaries. Set {LOGOS_BLOCKCHAIN_NODE_BIN}, \
                    or run `scripts/run/run-examples.sh host` to restore them into \
                    `testing-framework/assets/stack/bin`."
                    ),
                });
            }
        }

        Ok(())
    }

    // Construct a scenario builder with the appropriate configuration for the
    // expected deployer kind. This checks that the deployer kind matches the
    // expected kind, and then applies the world configuration (topology,
    // duration, workloads, expectations) to the builder.
    fn make_builder_for_deployer(
        &self,
        expected: DeployerKind,
    ) -> Result<ScenarioBuilderWith, StepError> {
        let actual = self.deployer.ok_or(StepError::MissingDeployer)?;
        if actual != expected {
            return Err(StepError::DeployerMismatch { expected, actual });
        }

        let topology = self
            .spec
            .topology
            .clone()
            .ok_or(StepError::MissingTopology)?;
        let duration_secs = self
            .spec
            .duration_secs
            .ok_or(StepError::MissingRunDuration)?
            .get();

        let mut builder: ScenarioBuilderWith = make_builder(&topology);

        builder = builder.with_run_duration(Duration::from_secs(duration_secs));
        if let Some(wallets) = self.spec.wallets {
            builder = builder.initialize_wallet(wallets.total_funds, wallets.users.get());
        }

        if let Some(tx) = self.spec.transactions {
            builder = builder.transactions_with(|flow| {
                let mut flow = flow.rate(tx.rate_per_block.get());
                if let Some(users) = tx.users {
                    flow = flow.users(users.get());
                }
                flow
            });
        }

        if let Some(liveness) = self.spec.consensus_liveness {
            if let Some(lag) = liveness.lag_allowance {
                builder = builder
                    .with_expectation(ConsensusLiveness::default().with_lag_allowance(lag.get()));
            } else {
                builder = builder.expect_consensus_liveness();
            }
        }

        Ok(builder)
    }

    // Helper to resolve a node name to the actual started node name. This is useful
    // for steps that refer to nodes by a logical name, and need to find the
    // corresponding started node in the world.
    pub fn resolve_node_name(&self, node_name: &str) -> Result<String, StepError> {
        Ok(self
            .nodes_info
            .get(node_name)
            .ok_or(StepError::LogicalError {
                message: format!("Runtime node '{node_name}' not found"),
            })?
            .started_node
            .name
            .clone())
    }
}
