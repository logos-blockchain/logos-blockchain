//! Converts shared network preparation into the existing Logos deployment plan.

use std::time::Duration;

use lb_config::create_general_configs_from_prepared_network;
use lb_key_management_system_service::keys::Key;

use super::{
    Config,
    deployment::{NODE_BINARY_PROFILE, TopologyBuildError},
    prepared_deployment::PreparedDeployment,
};
use crate::{DeploymentPlan, NodePlan, env::replace_default_env};

const PROLONGED_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(5);

pub fn build_plan(prepared: PreparedDeployment) -> Result<DeploymentPlan, TopologyBuildError> {
    let Some(network) = prepared.network else {
        return Ok(DeploymentPlan::new(prepared.config, Vec::new()));
    };

    let wallet_keys = prepared
        .config
        .wallet_config
        .accounts
        .iter()
        .map(|account| Key::Zk(account.secret_key.clone()))
        .collect::<Vec<_>>();
    let configs = create_general_configs_from_prepared_network(
        &network,
        prepared.config.network_params.as_ref(),
        PROLONGED_BOOTSTRAP_PERIOD,
        &wallet_keys,
    );
    let nodes = build_node_plans(prepared.config.n_nodes, network.node_ids(), &configs)?;

    let _unused = replace_default_env(
        NODE_BINARY_PROFILE,
        prepared.config.node_binary_profile.to_string(),
    );

    Ok(DeploymentPlan::new(prepared.config, nodes))
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
