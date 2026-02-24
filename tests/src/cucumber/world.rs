use std::{
    collections::HashMap,
    env,
    fmt::Debug,
    num::NonZero,
    path::{Path, PathBuf},
    time::Duration,
};

use cucumber::World;
use derivative::Derivative;
use lb_core::mantle::{SignedMantleTx, Utxo};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    LbcEnv, LbcManualCluster, ScenarioBuilder, ScenarioBuilderExt as _,
    configs::wallet::WalletAccount, workloads,
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
#[derivative(Default)]
pub struct CucumberWorld {
    /// The deployer kind that this scenario is configured for.
    pub deployer: Option<DeployerKind>,
    /// Base directory for scenario artifacts like logs and generated configs.
    pub scenario_base_dir: PathBuf,
    /// Automated: Scenario specification
    pub spec: ScenarioSpec,
    /// Automated: Runtime state for the scenario.
    pub run: RunState,
    /// Automated: Whether to perform membership checks on nodes after starting
    /// them, to verify they have joined the network as expected.
    pub membership_check: bool,
    /// Automated: Whether to perform readiness checks on nodes after starting
    /// them.
    pub readiness_checks: bool,
    /// Manual: List of genesis block UTXOs allocated in the genesis
    /// configuration.
    pub genesis_block_utxos: Vec<Utxo>,
    /// Manual: Optional local cluster instance for scenarios that use the local
    /// deployer.
    #[derivative(Default(value = "None"))]
    /// Manual: Mapping of logical node names to their corresponding node
    /// information, which includes the started node instance and any relevant
    /// metadata.
    pub local_cluster: Option<LbcManualCluster>,
    pub nodes_info: HashMap<String, NodeInfo>,
    /// Manual: List of genesis tokens allocated to wallets accounts.
    pub genesis_tokens: Vec<GenesisTokens>,
    /// Manual: Mapping of logical wallet names to their corresponding wallet
    /// resources.
    pub wallet_info: WalletInfoMap,
    /// Manual: Mapping of wallet account indices to their corresponding wallet
    /// account in the cluster.
    pub wallet_accounts: HashMap<usize, WalletAccount>,
    /// Manual: Mapping of logical wallet names to a mapping of chain height to
    /// the
    pub wallet_tokens_per_block: HashMap<String, WalletTokenMap>,
    /// Manual: Mapping of logical wallet names to the UTXOs that are currently
    /// encumbered (i.e. spent but not yet finalized) for that wallet.
    pub wallet_encumbered_tokens: HashMap<String, Vec<Utxo>>,
    /// Manual:  Per node: `header_id` -> height
    pub node_header_heights: HashMap<String, HashMap<String, u64>>,
    /// Manual: Mapping of logical node names to their corresponding libp2p peer
    /// IDs.
    pub node_peer_ids: HashMap<String, libp2p::PeerId>,
    /// Manual: Whether to populate the IBD peers for each node after starting
    /// them,
    pub populate_ibd_peers: Option<bool>,
    /// Manual: Whether to require all peers to be online after starting them.
    pub require_all_peers_mode_online_at_startup: Option<bool>,
}

/// Mapping of block header to the UTXOs and STXOs associated with a wallet in
/// that block.
#[derive(Debug)]
pub struct WalletTokenMap {
    /// The block hash.
    pub header_id: String,
    /// The UTXOs associated with the wallet for the block hash - this takes
    /// into account outputs that have been spent up to that block.
    pub utxos_per_wallet: HashMap<String, Vec<Utxo>>,
}

fn nodes_info_display(nodes_info: &HashMap<String, NodeInfo>) -> String {
    let nodes: Vec<_> = nodes_info
        .iter()
        .map(|(k, v)| {
            let wallets: Vec<_> = v
                .wallet_info
                .values()
                .map(|w| w.wallet_name.clone())
                .collect();
            let wallets_str = if wallets.is_empty() {
                "[]".to_owned()
            } else {
                format!("[{}]", wallets.join(", "))
            };
            format!("'{}: {} {}'", k, v.started_node.name, wallets_str)
        })
        .collect();
    format!("HashMap<String, NodeInfo>({})", nodes.join(", "))
}

fn wallet_info_display(wallet_info: &WalletInfoMap) -> String {
    let wallets: Vec<_> = wallet_info
        .iter()
        .map(|(k, v)| format!("'{}: {}'", k, v.wallet_name))
        .collect();
    format!("WalletInfoMap({})", wallets.join(", "))
}

fn wallet_accounts_display(wallet_accounts: &HashMap<usize, WalletAccount>) -> String {
    let accounts: Vec<_> = wallet_accounts
        .iter()
        .map(|(k, v)| format!("'{}: {:?} {:?} {:?}'", k, v.label, v.value, v.secret_key))
        .collect();
    format!("HashMap<usize, WalletAccount>({})", accounts.join(", "))
}

fn wallet_tokens_per_block_display(
    wallet_tokens_per_block: &HashMap<String, WalletTokenMap>,
) -> String {
    let blocks: Vec<_> = wallet_tokens_per_block
        .iter()
        .map(|(k, v)| {
            format!(
                "{}: {} {}",
                k,
                v.header_id,
                v.utxos_per_wallet
                    .iter()
                    .map(|v| format!("{}: [{}]", v.0, v.1.len()))
                    .collect::<Vec<_>>()
                    .join(" -")
            )
        })
        .collect();
    format!("HashMap<String, WalletTokenMap>({})", blocks.join(", "))
}

fn wallet_encumbered_tokens_display(
    wallet_encumbered_tokens: &HashMap<String, Vec<Utxo>>,
) -> String {
    let tokens: Vec<_> = wallet_encumbered_tokens
        .iter()
        .map(|(k, v)| format!("'{}: {}'", k, v.len()))
        .collect();
    format!("HashMap<String, Vec<Utxo>>({})", tokens.join(", "))
}

fn node_header_heights_display(
    node_header_heights: &HashMap<String, HashMap<String, u64>>,
) -> String {
    let nodes: Vec<_> = node_header_heights
        .iter()
        .map(|(k, v)| {
            format!(
                "{}: {}",
                k,
                v.iter()
                    .map(|v| format!("{}: {}", v.1, v.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    format!(
        "HashMap<String, HashMap<String, u64>>({})",
        nodes.join(", ")
    )
}

fn node_peer_ids_display(node_peer_ids: &HashMap<String, libp2p::PeerId>) -> String {
    let nodes: Vec<_> = node_peer_ids
        .iter()
        .map(|(k, v)| format!("'{k}: {v}'"))
        .collect();
    format!("HashMap<String, libp2p::PeerId>({})", nodes.join(", "))
}

impl Debug for CucumberWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CucumberWorld")
            .field("deployer", &format!("{:?}", self.deployer))
            .field("scenario_base_dir", &self.scenario_base_dir)
            .field("spec", &format!("{:?}", self.spec))
            .field("run", &format!("{:?}", self.run))
            .field("membership_check", &self.membership_check)
            .field("readiness_checks", &self.readiness_checks)
            .field(
                "populate_ibd_peers",
                &format!("{:?}", self.populate_ibd_peers),
            )
            .field(
                "require_all_peers_mode_online_at_startup",
                &format!("{:?}", self.require_all_peers_mode_online_at_startup),
            )
            .field(
                "genesis_block_utxos",
                &format!("{:?}", self.genesis_block_utxos),
            )
            .field("local_cluster", {
                if self.local_cluster.is_some() {
                    &"Has LbcManualCluster"
                } else {
                    &"None"
                }
            })
            .field("nodes_info", &nodes_info_display(&self.nodes_info))
            .field("genesis_tokens", &format!("{:?}", self.genesis_tokens))
            .field("wallet_info", &wallet_info_display(&self.wallet_info))
            .field(
                "wallet_accounts",
                &wallet_accounts_display(&self.wallet_accounts),
            )
            .field(
                "wallet_tokens_per_block",
                &wallet_tokens_per_block_display(&self.wallet_tokens_per_block),
            )
            .field(
                "wallet_encumbered_tokens",
                &wallet_encumbered_tokens_display(&self.wallet_encumbered_tokens),
            )
            .field(
                "node_header_heights",
                &node_header_heights_display(&self.node_header_heights),
            )
            .field("node_peer_ids", &node_peer_ids_display(&self.node_peer_ids))
            .finish()
    }
}

/// Information about genesis tokens allocated to a wallet account in the world.
#[derive(Clone, Debug)]
pub struct GenesisTokens {
    /// The account index in the genesis tokens that this allocation corresponds
    /// to.
    pub account_index: usize,
    /// The number of tokens allocated to this account in the genesis
    /// configuration.
    pub token_count: usize,
    /// The total amount of tokens allocated to this account in the genesis
    /// configuration.
    pub token_amount: u64,
}

/// Information about a wallet resource created in the world, which can be used
/// to track and reference wallets across steps.
#[derive(Clone, Debug)]
pub struct WalletInfo {
    /// Logical name of the wallet resource, used for referencing in steps.
    pub wallet_name: String,
    /// Logical name of the node resource where this wallet is referenced.
    pub node_name: String,
    /// The account index in the genesis tokens that this resource corresponds
    /// to.
    pub account_index: usize,
    /// The actual wallet account in the cluster that this resource corresponds
    /// to.
    pub wallet_account: WalletAccount,
}

/// Mapping of chain height to the corresponding block hash at that height.
pub type ChainInfoMap = HashMap<u64, String>;
/// Mapping of logical wallet names to their corresponding wallet information.
pub type WalletInfoMap = HashMap<String, WalletInfo>;

/// Information about a started node in the world
pub struct NodeInfo {
    /// Node name
    pub name: String,
    /// The actual started node instance
    pub started_node: StartedNode<LbcEnv>,
    /// General node configuration used to start the node
    pub run_config: Option<RunConfig>,
    /// Chain height vs. hash at that height
    pub chain_info: ChainInfoMap,
    /// The wallets associated with this node.
    pub wallet_info: WalletInfoMap,
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

    /// Helper to resolve a node name to the actual started node name. This is
    /// useful for steps that refer to nodes by a logical name, and need to
    /// find the corresponding started node in the world.
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

    /// Helper to resolve wallet names to the actual wallet information. This
    /// is useful for steps that refer to wallets by a logical name, and
    /// need to find the corresponding wallet information in the world.
    pub fn resolve_wallet(&self, wallet_name: &str) -> Result<WalletInfo, StepError> {
        self.resolve_wallets(&[wallet_name.to_owned()])?
            .into_iter()
            .next()
            .ok_or(StepError::MissingWallet)
    }

    /// Helper to resolve wallet names to the actual wallet information. This
    /// is useful for steps that refer to wallets by a logical name, and
    /// need to find the corresponding wallet information in the world.
    pub fn resolve_wallets(&self, wallet_names: &[String]) -> Result<Vec<WalletInfo>, StepError> {
        wallet_names
            .iter()
            .map(|w| {
                self.wallet_info
                    .get(w)
                    .cloned()
                    .ok_or(StepError::LogicalError {
                        message: format!("Wallet '{w}' not found in world state"),
                    })
            })
            .collect::<Result<Vec<_>, _>>()
    }

    /// Helper to submit a transaction to the node associated with the given
    /// wallet. This abstracts away the details of finding the correct node
    /// and using its client.
    pub async fn submit_transaction(
        &self,
        wallet: &WalletInfo,
        signed_tx: &SignedMantleTx,
    ) -> Result<(), StepError> {
        let node = self
            .nodes_info
            .get(&wallet.node_name)
            .ok_or(StepError::LogicalError {
                message: format!(
                    "Node '{}' for wallet '{}' not found",
                    wallet.node_name, wallet.wallet_name
                ),
            })?;
        tokio::time::timeout(
            Duration::from_secs(10),
            node.started_node.client.submit_transaction(signed_tx),
        )
        .await
        .map_err(|_| StepError::Timeout {
            message: format!(
                "Submit transaction '{}/{}' ",
                wallet.wallet_name, wallet.node_name
            ),
        })??;

        Ok(())
    }
}
