use std::{collections::HashMap, env, num::NonZero, path::PathBuf, time::Duration};

use cucumber::World;
use derivative::Derivative;
use lb_node::config::RunConfig;
use testing_framework_core::scenario::{Builder, NodeControlCapability, Scenario, StartedNode};
use testing_framework_runner_local::LocalManualCluster;
use testing_framework_workflows::{ScenarioBuilderExt as _, expectations::ConsensusLiveness};

use crate::{
    cucumber::{
        error::{StepError, StepResult},
        utils::{make_builder, shared_host_bin_path},
    },
    non_zero,
};

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
    pub duration_secs: Option<NonZero<u64>>,
    pub wallets: Option<WalletSpec>,
    pub transactions: Option<TransactionSpec>,
    pub consensus_liveness: Option<ConsensusLivenessSpec>,
}

#[derive(Debug, Clone, Copy)]
pub struct TopologySpec {
    pub validators: NonZero<usize>,
    pub network: NetworkKind,
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
    pub local_cluster: Option<LocalManualCluster>,
    #[derivative(Debug = "ignore")]
    pub nodes_info: HashMap<String, NodeInfo>,
}

pub type ChainInfoMap = HashMap<u64, String>;

/// Information about a started node in the world
pub struct NodeInfo {
    /// Node name
    pub name: String,
    pub started_node: StartedNode,
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
    pub fn node_best_height(&self, node_name: &String) -> Result<Option<u64>, StepError> {
        let node = self
            .nodes_info
            .get(node_name)
            .ok_or(StepError::LogicalError {
                message: format!("Runtime node '{node_name}' not found"),
            })?;
        Ok(node.best_height())
    }

    pub const fn set_deployer(&mut self, kind: DeployerKind) -> StepResult {
        self.deployer = Some(kind);
        Ok(())
    }

    pub fn set_topology(&mut self, validators: usize, network: NetworkKind) -> StepResult {
        self.spec.topology = Some(TopologySpec {
            validators: non_zero!("validators", validators)?,
            network,
        });
        Ok(())
    }

    pub fn set_run_duration(&mut self, seconds: u64) -> StepResult {
        self.spec.duration_secs = Some(non_zero!("duration", seconds)?);
        Ok(())
    }

    pub fn set_wallets(&mut self, total_funds: u64, users: usize) -> StepResult {
        self.spec.wallets = Some(WalletSpec {
            total_funds,
            users: non_zero!("wallet users", users)?,
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

        self.spec.transactions = Some(TransactionSpec {
            rate_per_block: non_zero!("transactions rate", rate_per_block)?,
            users: match users {
                Some(val) => Some(non_zero!("transactions users", val)?),
                None => None,
            },
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
        self.spec.consensus_liveness = Some(ConsensusLivenessSpec {
            lag_allowance: Some(non_zero!("lag allowance", blocks)?),
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

        if expected == DeployerKind::Local {
            let node_ok = env::var_os("LOGOS_BLOCKCHAIN_NODE_BIN")
                .map(PathBuf::from)
                .is_some_and(|p| p.is_file())
                || shared_host_bin_path("logos-blockchain-node").is_file();

            if !(node_ok) {
                return Err(StepError::Preflight {
                    message: "Missing Logos host binaries. Set LOGOS_BLOCKCHAIN_NODE_BIN, or run \
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
            .ok_or(StepError::MissingRunDuration)?
            .get();

        let mut builder: Builder<Caps> = make_builder(topology).with_capabilities(Caps::default());

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
