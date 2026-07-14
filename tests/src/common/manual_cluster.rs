use std::{
    fs,
    num::NonZero,
    path::{Path, PathBuf},
    time::Duration,
};

use lb_core::mantle::{GenesisTx as _, Utxo};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_libp2p::Multiaddr;
use lb_node::{UserConfig, config::RunConfig};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, LbcLocalDeployer, LbcManualCluster, NodeHttpClient, TopologyConfig,
    USER_CONFIG_FILE, configs::wallet::WalletConfig, internal::DeploymentPlan, is_truthy_env,
    record_system_monitor_event, register_system_monitor_output_file,
    unregister_system_monitor_output_file,
};
use lb_utils::math::NonNegativeRatio;
use lb_zone_sdk::Slot;
use reqwest::Url;
use tempfile::TempDir;
use testing_framework_core::scenario::{DynError, PeerSelection, StartNodeOptions, StartedNode};
use tokio::time::error::Elapsed;

use crate::cucumber::defaults::{E2E_ARTIFACTS_DIR, E2E_KEEP_LOGS, E2E_TESTS_BASE_DIR_OVERRIDE};

pub struct LocalManualClusterHarnessBase {
    scenario_base_dir: PathBuf,
    deployment: DeploymentPlan,
    cluster: LbcManualCluster,
    _scenario_base_dir_guard: ScenarioBaseDirGuard,
}

impl LocalManualClusterHarnessBase {
    pub fn scenario_base_dir(&self) -> &Path {
        &self.scenario_base_dir
    }

    pub const fn deployment(&self) -> &DeploymentPlan {
        &self.deployment
    }

    pub const fn cluster(&self) -> &LbcManualCluster {
        &self.cluster
    }

    pub const fn cluster_mut(&mut self) -> &mut LbcManualCluster {
        &mut self.cluster
    }
}

struct ScenarioBaseDirGuard {
    tempdir: Option<TempDir>,
    system_monitor_output_path: PathBuf,
}

impl Drop for ScenarioBaseDirGuard {
    fn drop(&mut self) {
        unregister_system_monitor_output_file(&self.system_monitor_output_path);

        if (std::thread::panicking() || e2e_keep_logs_enabled())
            && let Some(tempdir) = self.tempdir.take()
        {
            let _kept_path = tempdir.keep();
            return;
        }

        if let Some(tempdir) = self.tempdir.take()
            && has_artifacts_beyond_system_monitor_outputs(tempdir.path())
        {
            let _kept_path = tempdir.keep();
        }
    }
}

fn e2e_keep_logs_enabled() -> bool {
    is_truthy_env(E2E_KEEP_LOGS)
}

// If the directory only contains system monitor leftovers, clean it up.
// Preserve it when other scenario artifacts were produced so they remain
// inspectable.
fn has_artifacts_beyond_system_monitor_outputs(path: &Path) -> bool {
    fs::read_dir(path).map_or(true, |entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !matches!(name, "system_stats.ndjson" | "system_stats.csv"))
        })
    })
}

#[derive(Debug, Clone, Copy)]
pub enum ManualNodeLayout {
    SelectNodeSeed(usize),
}

#[must_use]
pub fn build_local_manual_cluster(
    test_name: &str,
    prefix: &str,
    builder: DeploymentBuilder,
    scenario_dir_override: Option<PathBuf>,
) -> LocalManualClusterHarnessBase {
    let (scenario_base_dir, scenario_base_dir_guard) =
        managed_scenario_base_dir(&format!("{prefix}-{test_name}"), scenario_dir_override);
    register_system_monitor_output_file(&scenario_base_dir.join("system_stats.ndjson"));
    record_system_monitor_event(
        "manual_cluster_prepared",
        scenario_base_dir.display().to_string(),
    );

    let deployment = builder
        .scenario_base_dir(scenario_base_dir.clone())
        .build()
        .expect("manual-cluster deployment should build");

    let cluster = LbcLocalDeployer::new().manual_cluster_from_descriptors(deployment.clone());
    let system_monitor_output_path = scenario_base_dir.join("system_stats.ndjson");

    LocalManualClusterHarnessBase {
        scenario_base_dir,
        deployment,
        cluster,
        _scenario_base_dir_guard: ScenarioBaseDirGuard {
            tempdir: Some(scenario_base_dir_guard),
            system_monitor_output_path,
        },
    }
}

pub async fn start_local_manual_cluster_with_layout<F>(
    test_name: &str,
    prefix: &str,
    builder: DeploymentBuilder,
    node_count: usize,
    layout: ManualNodeLayout,
    config_patch: F,
    scenario_dir_override: Option<PathBuf>,
) -> (LocalManualClusterHarnessBase, Vec<StartedNode<LbcEnv>>)
where
    F: Fn(RunConfig) -> Result<RunConfig, DynError> + Clone + Send + Sync + 'static,
{
    let base = build_local_manual_cluster(test_name, prefix, builder, scenario_dir_override);

    let nodes = start_manual_nodes_with_layout(
        &base.cluster,
        &base.scenario_base_dir,
        node_count,
        layout,
        config_patch,
    )
    .await;

    base.cluster
        .wait_network_ready()
        .await
        .expect("manual cluster should become ready");

    (base, nodes)
}

/// Starts a local cluster of `node_count` nodes with fast blocks (2s slots)
/// and the given wallet accounts funded at genesis. Artifacts are written
/// under [`E2E_ARTIFACTS_DIR`].
pub async fn start_fast_cluster_with_wallet(
    test_name: &str,
    prefix: &str,
    node_count: usize,
    wallet_config: WalletConfig,
) -> (LocalManualClusterHarnessBase, Vec<StartedNode<LbcEnv>>) {
    start_local_manual_cluster_with_layout(
        test_name,
        prefix,
        DeploymentBuilder::new(
            TopologyConfig::with_node_numbers(node_count)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some(test_name.to_owned())),
        )
        .with_wallet_config(wallet_config),
        node_count,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok(fast_chain_config(config)),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await
}

fn fast_chain_config(mut config: RunConfig) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(2);
    config.deployment.cryptarchia.security_param = NonZero::new(3).expect("nonzero");
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().expect("nonzero"));
    config
}

pub async fn start_manual_nodes_with_layout<F>(
    cluster: &LbcManualCluster,
    scenario_base_dir: &Path,
    node_count: usize,
    layout: ManualNodeLayout,
    config_patch: F,
) -> Vec<StartedNode<LbcEnv>>
where
    F: Fn(RunConfig) -> Result<RunConfig, DynError> + Clone + Send + Sync + 'static,
{
    let mut nodes: Vec<StartedNode<LbcEnv>> = Vec::with_capacity(node_count);
    let start_order = start_order(node_count, layout);

    for (start_position, node_index) in start_order.into_iter().enumerate() {
        let peers = peers_for_node(&nodes, start_position, layout);

        nodes.push(
            Box::pin(
                cluster.start_node_with(
                    &node_index.to_string(),
                    StartNodeOptions::default()
                        .with_peers(peers)
                        .with_persist_dir(scenario_base_dir.join(format!("node-{node_index}")))
                        .create_patch(config_patch.clone()),
                ),
            )
            .await
            .unwrap_or_else(|error| panic!("starting node-{node_index} should succeed: {error}")),
        );
    }

    nodes
}

fn start_order(node_count: usize, layout: ManualNodeLayout) -> Vec<usize> {
    match layout {
        ManualNodeLayout::SelectNodeSeed(seed_index) => {
            assert!(
                seed_index < node_count,
                "seed node index {seed_index} is out of range for {node_count} nodes",
            );

            std::iter::once(seed_index)
                .chain((0..node_count).filter(move |node_index| *node_index != seed_index))
                .collect()
        }
    }
}

fn peers_for_node(
    nodes: &[StartedNode<LbcEnv>],
    start_position: usize,
    layout: ManualNodeLayout,
) -> PeerSelection {
    match layout {
        ManualNodeLayout::SelectNodeSeed(_) => {
            if start_position == 0 {
                PeerSelection::None
            } else {
                PeerSelection::Named(vec![nodes[0].name.clone()])
            }
        }
    }
}

pub async fn wait_for_height(
    client: &NodeHttpClient,
    target_height: u64,
    duration: Duration,
) -> Result<(), Elapsed> {
    tokio::time::timeout(duration, async {
        loop {
            if let Ok(info) = client.consensus_info().await
                && info.cryptarchia_info.height >= target_height
            {
                return;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
}

pub async fn wait_for_nodes_height(
    nodes: &[&NodeHttpClient],
    target_height: u64,
    duration: Duration,
) {
    for node in nodes {
        wait_for_height(node, target_height, duration)
            .await
            .unwrap_or_else(|_| panic!("node should reach height {target_height}"));
    }
}

pub async fn wait_for_tip_slot(
    client: &NodeHttpClient,
    slot: Slot,
    timeout: Duration,
) -> Result<(), Elapsed> {
    tokio::time::timeout(timeout, async {
        loop {
            if let Ok(info) = client.consensus_info().await
                && info.cryptarchia_info.slot >= slot
            {
                return;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
}

pub async fn wait_for_nodes_tip_slot(nodes: &[&NodeHttpClient], slot: Slot, duration: Duration) {
    for node in nodes {
        wait_for_tip_slot(node, slot, duration)
            .await
            .unwrap_or_else(|_| panic!("node should reach tip slot {slot:?}"));
    }
}

pub async fn get_wallet_balance(node: &NodeHttpClient, pk: ZkPublicKey) -> u64 {
    let pk_hex = hex::encode(lb_groth16::fr_to_bytes(&pk.into()));
    let url = api_url(node, &format!("wallet/{pk_hex}/balance"));

    for _ in 0..5 {
        let response = reqwest::Client::new()
            .get(url.clone())
            .send()
            .await
            .expect("balance request should not fail");

        if response.status().is_success() {
            let body: serde_json::Value = response.json().await.unwrap();
            return body["balance"].as_u64().unwrap_or(0);
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    panic!("failed to get wallet balance after retries");
}

#[must_use]
pub fn api_url(node: &NodeHttpClient, path: &str) -> Url {
    node.base_url()
        .join(path)
        .expect("manual-cluster client base URL should join API path")
}

/// UTXOs created by the genesis transfer of the deployment's genesis block.
#[must_use]
pub fn genesis_wallet_utxos(config: &TopologyConfig) -> Vec<Utxo> {
    let genesis_tx = config
        .genesis_block
        .as_ref()
        .expect("manual-cluster deployment should include genesis tx")
        .genesis_tx();
    let genesis_transfer = genesis_tx.genesis_transfer();

    genesis_transfer.outputs.utxos(genesis_transfer).collect()
}

#[must_use]
pub fn unique_scenario_base_dir(label: &str, scenario_dir_override: Option<PathBuf>) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0u128, |duration| duration.as_nanos());

    let unique_scenario_base_dir =
        scenario_base_root_dir(scenario_dir_override).join(format!("{label}-{nanos}"));

    println!(
        "unique_scenario_base_dir: '{}'",
        unique_scenario_base_dir.display()
    );
    unique_scenario_base_dir
}

fn managed_scenario_base_dir(
    label: &str,
    scenario_dir_override: Option<PathBuf>,
) -> (PathBuf, TempDir) {
    let base_root_dir = scenario_base_root_dir(scenario_dir_override);
    fs::create_dir_all(&base_root_dir).unwrap_or_else(|error| {
        panic!(
            "failed to create scenario base root '{}': {error}",
            base_root_dir.display()
        )
    });

    let scenario_base_dir_guard = tempfile::Builder::new()
        .prefix(&format!("{label}-"))
        .tempdir_in(&base_root_dir)
        .unwrap_or_else(|error| {
            panic!(
                "failed to create scenario base directory in '{}': {error}",
                base_root_dir.display()
            )
        });
    let scenario_base_dir = scenario_base_dir_guard.path().to_path_buf();

    println!(
        "unique_scenario_base_dir: '{}'",
        scenario_base_dir.display()
    );

    (scenario_base_dir, scenario_base_dir_guard)
}

fn scenario_base_root_dir(scenario_dir_override: Option<PathBuf>) -> PathBuf {
    if std::env::var_os(E2E_TESTS_BASE_DIR_OVERRIDE).is_some() {
        e2e_tests_base_dir_from_env_override()
    } else if let Some(dir_override) = scenario_dir_override {
        dir_override
    } else {
        std::env::temp_dir()
    }
}

fn e2e_tests_base_dir_from_env_override() -> PathBuf {
    match std::env::var_os(E2E_TESTS_BASE_DIR_OVERRIDE) {
        Some(raw) if !raw.is_empty() => PathBuf::from(raw),
        Some(raw) => {
            let temp_dir = std::env::temp_dir();
            println!(
                "Invalid value for {E2E_TESTS_BASE_DIR_OVERRIDE}: '{}', using '{}'",
                raw.display(),
                temp_dir.display(),
            );
            temp_dir
        }
        None => std::env::temp_dir(),
    }
}

#[must_use]
pub fn read_manual_node_logs(base_dir: &Path, runtime_node_name: &str) -> String {
    runtime_dirs_for_node(base_dir, runtime_node_name)
        .into_iter()
        .flat_map(|runtime_dir| log_files_for_node(&runtime_dir, runtime_node_name))
        .map(|path| read_log_file(&path))
        .collect()
}

pub fn override_node_initial_peers(
    base_dir: &Path,
    runtime_node_name: &str,
    initial_peers: Vec<Multiaddr>,
) {
    let runtime_dir = runtime_dir_for_node(base_dir, runtime_node_name);
    let mut user_config = read_user_config(&runtime_dir);

    user_config.network.backend.initial_peers = initial_peers;

    write_user_config(&runtime_dir, &user_config);
}

fn runtime_dirs_for_node(base_dir: &Path, runtime_node_name: &str) -> Vec<PathBuf> {
    let runtime_dir_prefix = format!("{runtime_node_name}_");

    read_dir_paths(base_dir)
        .into_iter()
        .filter(|path| is_runtime_dir_for_node(path, &runtime_dir_prefix))
        .collect()
}

fn log_files_for_node(runtime_dir: &Path, runtime_node_name: &str) -> Vec<PathBuf> {
    let log_file_prefix = format!("__logs-{runtime_node_name}");

    read_dir_paths(runtime_dir)
        .into_iter()
        .filter(|path| is_log_file_for_node(path, &log_file_prefix))
        .collect()
}

fn read_dir_paths(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap_or_else(|source| panic!("failed to read directory {}: {source}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect()
}

fn is_runtime_dir_for_node(path: &Path, runtime_dir_prefix: &str) -> bool {
    path.is_dir()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(runtime_dir_prefix))
}

fn is_log_file_for_node(path: &Path, log_file_prefix: &str) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(log_file_prefix))
}

fn read_log_file(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|source| panic!("failed to read log file {}: {source}", path.display()))
}

fn runtime_dir_for_node(base_dir: &Path, runtime_node_name: &str) -> PathBuf {
    let runtime_dir_prefix = format!("{runtime_node_name}_");

    read_dir_paths(base_dir)
        .into_iter()
        .find(|path| is_runtime_dir_for_node(path, &runtime_dir_prefix))
        .unwrap_or_else(|| {
            panic!(
                "failed to locate runtime dir for node `{runtime_node_name}` under {}",
                base_dir.display()
            )
        })
}

fn read_user_config(persist_dir: &Path) -> UserConfig {
    let path = persist_dir.join(USER_CONFIG_FILE);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|source| panic!("failed to read node config {}: {source}", path.display()));

    serde_yaml::from_str(&text)
        .unwrap_or_else(|source| panic!("failed to parse node config {}: {source}", path.display()))
}

fn write_user_config(persist_dir: &Path, config: &UserConfig) {
    let path = persist_dir.join(USER_CONFIG_FILE);
    let yaml = serde_yaml::to_string(config).unwrap_or_else(|source| {
        panic!(
            "failed to serialize node config {}: {source}",
            path.display()
        )
    });

    fs::write(&path, yaml).unwrap_or_else(|source| {
        panic!("failed to write node config {}: {source}", path.display())
    });
}
