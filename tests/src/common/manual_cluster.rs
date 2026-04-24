use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use lb_key_management_system_service::keys::ZkPublicKey;
use lb_libp2p::Multiaddr;
use lb_node::{UserConfig, config::RunConfig};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, LbcLocalDeployer, LbcManualCluster, NodeHttpClient,
    USER_CONFIG_FILE, internal::DeploymentPlan,
};
use reqwest::Url;
use testing_framework_core::scenario::{DynError, PeerSelection, StartNodeOptions, StartedNode};
use tokio::time::error::Elapsed;

use crate::nodes::get_exe_path;

pub struct LocalManualClusterHarnessBase {
    pub scenario_base_dir: PathBuf,
    pub deployment: DeploymentPlan,
    pub cluster: LbcManualCluster,
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
) -> LocalManualClusterHarnessBase {
    ensure_local_node_binary_env();

    let scenario_base_dir = unique_scenario_base_dir(&format!("{prefix}-{test_name}"));
    let deployment = builder
        .scenario_base_dir(scenario_base_dir.clone())
        .build()
        .expect("manual-cluster deployment should build");

    let cluster = LbcLocalDeployer::new().manual_cluster_from_descriptors(deployment.clone());

    LocalManualClusterHarnessBase {
        scenario_base_dir,
        deployment,
        cluster,
    }
}

pub async fn start_local_manual_cluster_with_layout<F>(
    test_name: &str,
    prefix: &str,
    builder: DeploymentBuilder,
    node_count: usize,
    layout: ManualNodeLayout,
    config_patch: F,
) -> (LocalManualClusterHarnessBase, Vec<StartedNode<LbcEnv>>)
where
    F: Fn(RunConfig) -> Result<RunConfig, DynError> + Clone + Send + Sync + 'static,
{
    let base = build_local_manual_cluster(test_name, prefix, builder);

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

pub fn ensure_local_node_binary_env() {
    // Respect an existing binary override (for example, a testing-featured build).
    if std::env::var_os("LOGOS_BLOCKCHAIN_NODE_BIN").is_some() {
        return;
    }

    // SAFETY: Tests set this process-local env var before spawning node processes.
    // We do not read-modify-write shared data through references here.
    unsafe {
        std::env::set_var("LOGOS_BLOCKCHAIN_NODE_BIN", get_exe_path());
    }
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
            .unwrap_or_else(|_| panic!("starting node-{node_index} should succeed")),
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
                && info.height >= target_height
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

#[must_use]
pub fn unique_scenario_base_dir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0u128, |duration| duration.as_nanos());

    std::env::temp_dir().join(format!("{label}-{nanos}"))
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
