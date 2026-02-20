use std::{collections::HashMap, error::Error, path::PathBuf, sync::Arc, time::Duration};

use lb_core::mantle::genesis_tx::GenesisTx;
use lb_node::config::RunConfig;
use lb_utils::net::get_available_udp_port;
use rand::{Rng, SeedableRng as _};
use testing_framework_core::topology::{DeploymentProvider, DeploymentSeed, DynTopologyError};
use thiserror::Error;

use super::{
    Libp2pNetworkLayout, NetworkParams,
    wallet::{WalletConfig, WalletConfigError},
};
use crate::node::{
    DeploymentPlan, NodePlan,
    configs::{Config, create_node_configs_from_ids, key_id_for_preload_backend, postprocess},
};

pub type DynError = Box<dyn Error + Send + Sync + 'static>;
const DEFAULT_SLOT_TIME_IN_SECS: u64 = 1;
const DEFAULT_ACTIVE_SLOT_COEFF: f64 = 1.0;
const DEFAULT_SECURITY_PARAM: u32 = 10;

#[derive(Debug, Error)]
pub enum TopologyBuildError {
    #[error("internal config vector mismatch for {label} (expected {expected}, got {actual})")]
    VectorLenMismatch {
        label: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("failed to allocate blend UDP ports for topology")]
    BlendPortAllocation,
    #[error(transparent)]
    InvalidWallet(#[from] WalletConfigError),
}

/// High-level topology settings used to generate node configs for a scenario.
#[derive(Clone)]
pub struct TopologyConfig {
    pub n_nodes: usize,
    pub network_params: Arc<NetworkParams>,
    pub wallet_config: WalletConfig,
    pub scenario_base_dir: PathBuf,
    pub genesis_tx: Option<GenesisTx>,
    pub slot_duration: Option<Duration>,
    pub active_slot_coeff: f64,
    pub security_param: u32,
    node_config_overrides: HashMap<usize, RunConfig>,
}

impl TopologyConfig {
    fn with_node_count(nodes: usize) -> Self {
        Self {
            n_nodes: nodes,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::with_node_count(0)
    }

    #[must_use]
    pub fn with_node_numbers(nodes: usize) -> Self {
        Self::with_node_count(nodes)
    }

    #[must_use]
    pub fn node_config_override(&self, index: usize) -> Option<&RunConfig> {
        self.node_config_overrides.get(&index)
    }
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            n_nodes: 0,
            network_params: Arc::new(NetworkParams::default()),
            wallet_config: WalletConfig::default(),
            scenario_base_dir: std::env::temp_dir(),
            genesis_tx: None,
            slot_duration: Some(Duration::from_secs(DEFAULT_SLOT_TIME_IN_SECS)),
            active_slot_coeff: DEFAULT_ACTIVE_SLOT_COEFF,
            security_param: DEFAULT_SECURITY_PARAM,
            node_config_overrides: HashMap::new(),
        }
    }
}

/// Deployment-facing builder.
#[derive(Clone)]
pub struct DeploymentBuilder {
    config: TopologyConfig,
    seed: Option<DeploymentSeed>,
}

impl DeploymentBuilder {
    #[must_use]
    pub const fn new(config: TopologyConfig) -> Self {
        Self { config, seed: None }
    }

    #[must_use]
    pub const fn with_deployment_seed(mut self, seed: DeploymentSeed) -> Self {
        self.seed = Some(seed);
        self
    }

    #[must_use]
    pub fn with_node_config_override(mut self, index: usize, config: RunConfig) -> Self {
        self.config.node_config_overrides.insert(index, config);
        self
    }

    #[must_use]
    pub const fn with_node_count(mut self, nodes: usize) -> Self {
        self.config.n_nodes = nodes;
        self
    }

    #[must_use]
    pub const fn nodes(self, nodes: usize) -> Self {
        self.with_node_count(nodes)
    }

    #[must_use]
    pub fn scenario_base_dir(mut self, dir: PathBuf) -> Self {
        self.config.scenario_base_dir = dir;
        self
    }

    #[must_use]
    pub fn with_network_layout(mut self, layout: Libp2pNetworkLayout) -> Self {
        self.config.network_params = Arc::new(NetworkParams {
            libp2p_network_layout: layout,
        });
        self
    }

    #[must_use]
    pub fn with_wallet_config(mut self, wallet: WalletConfig) -> Self {
        self.config.wallet_config = wallet;
        self
    }

    pub fn build(mut self) -> Result<DeploymentPlan, TopologyBuildError> {
        self.config.wallet_config.validate()?;

        let node_count = self.config.n_nodes;
        if node_count == 0 {
            return Ok(DeploymentPlan::new(self.config, Vec::new()));
        }

        let ids = generate_node_ids(node_count, self.seed.as_ref());

        let blend_ports = allocate_blend_ports(node_count)?;
        let (mut node_configs, base_genesis_tx) = create_node_configs_from_ids(
            &ids,
            &blend_ports,
            node_count,
            self.config.network_params.as_ref(),
        );

        let wallet_accounts = self
            .config
            .wallet_config
            .accounts
            .iter()
            .map(|account| (account.secret_key.clone(), account.value))
            .collect::<Vec<_>>();

        let genesis_tx = postprocess::apply_wallet_genesis_overrides(
            &mut node_configs,
            &base_genesis_tx,
            &wallet_accounts,
            key_id_for_preload_backend,
        );

        let nodes = build_node_plans(node_count, &ids, &node_configs)?;
        self.config.genesis_tx = Some(genesis_tx);

        Ok(DeploymentPlan::new(self.config, nodes))
    }
}

fn allocate_blend_ports(node_count: usize) -> Result<Vec<u16>, TopologyBuildError> {
    let mut ports = Vec::with_capacity(node_count);

    for _ in 0..node_count {
        let Some(port) = get_available_udp_port() else {
            return Err(TopologyBuildError::BlendPortAllocation);
        };
        ports.push(port);
    }

    Ok(ports)
}

fn generate_node_ids(node_count: usize, seed: Option<&DeploymentSeed>) -> Vec<[u8; 32]> {
    let mut ids = vec![[0; 32]; node_count];
    if let Some(seed) = seed {
        let mut rng = rand::rngs::StdRng::from_seed(*seed.bytes());
        fill_node_ids(&mut ids, &mut rng);
        return ids;
    }

    let mut rng = rand::thread_rng();
    fill_node_ids(&mut ids, &mut rng);

    ids
}

fn fill_node_ids<R>(ids: &mut [[u8; 32]], rng: &mut R)
where
    R: Rng + ?Sized,
{
    for id in ids {
        rng.fill(id);
    }
}

fn build_node_plans(
    node_count: usize,
    ids: &[[u8; 32]],
    node_configs: &[Config],
) -> Result<Vec<NodePlan>, TopologyBuildError> {
    ensure_vector_len("ids", node_count, ids.len())?;
    ensure_vector_len("node_configs", node_count, node_configs.len())?;

    Ok(ids
        .iter()
        .copied()
        .zip(node_configs.iter().cloned())
        .enumerate()
        .map(|(index, (id, general))| NodePlan { index, id, general })
        .collect())
}

const fn ensure_vector_len(
    label: &'static str,
    expected: usize,
    actual: usize,
) -> Result<(), TopologyBuildError> {
    if expected == actual {
        return Ok(());
    }

    Err(TopologyBuildError::VectorLenMismatch {
        label,
        expected,
        actual,
    })
}

impl DeploymentProvider<DeploymentPlan> for DeploymentBuilder {
    fn build(&self, seed: Option<&DeploymentSeed>) -> Result<DeploymentPlan, DynTopologyError> {
        let builder = seed.map_or_else(
            || self.clone(),
            |seed| self.clone().with_deployment_seed(seed.clone()),
        );

        builder
            .build()
            .map_err(|error| Box::new(error) as DynTopologyError)
    }
}
