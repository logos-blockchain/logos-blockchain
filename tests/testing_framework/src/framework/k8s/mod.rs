mod assets;

use std::{
    collections::BTreeMap,
    env, process,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use lb_libp2p::Protocol;
use reqwest::Url;
use testing_framework_core::{cfgsync::build_node_artifact_override, scenario::DynError};
use testing_framework_runner_k8s::{
    K8sDeployEnv, NodeGroup, NodePortValues, NodeValues, PortSpecs, SharedServiceFileSpec,
    SharedServiceSpec, wait::NodeConfigPorts,
};

use super::{
    LbcEnv,
    constants::cfgsync_port,
    image::{resolve_k8s_bootstrap_image, resolve_k8s_node_image},
};
use crate::{
    env as tf_env,
    internal::{DeploymentPlan, NodePlan},
};

const K8S_FULLNAME_OVERRIDE: &str = "logos-runner";
const LOGOS_RUNNER_MOUNT_PATH: &str = "/etc/logos";
const LOGOS_K8S_NODE_IMAGE_PULL_POLICY: &str = "LOGOS_BLOCKCHAIN_K8S_NODE_IMAGE_PULL_POLICY";
const LOGOS_K8S_BOOTSTRAP_IMAGE_PULL_POLICY: &str =
    "LOGOS_BLOCKCHAIN_K8S_BOOTSTRAP_IMAGE_PULL_POLICY";

#[async_trait]
impl K8sDeployEnv for LbcEnv {
    type Assets = assets::K8sAssets;

    fn collect_port_specs(topology: &Self::Deployment) -> PortSpecs {
        let nodes = topology
            .nodes()
            .iter()
            .map(|node| NodeConfigPorts {
                api: node.general.api_config.address.port(),
                auxiliary: node.general.api_config.testing_http_address.port(),
            })
            .collect();
        PortSpecs { nodes }
    }

    fn prepare_assets(
        topology: &Self::Deployment,
        metrics_otlp_ingest_url: Option<&Url>,
    ) -> Result<Self::Assets, DynError> {
        assets::prepare_assets(topology, metrics_otlp_ingest_url).map_err(Into::into)
    }

    fn cluster_identifiers() -> (String, String) {
        match (
            env::var("LOGOS_K8S_NAMESPACE").ok(),
            env::var("LOGOS_K8S_RELEASE").ok(),
        ) {
            (Some(namespace), Some(release)) => (namespace, release),
            (Some(namespace), None) => (namespace, String::from("logos-k8s-smoke")),
            (None, Some(release)) => (default_namespace(), release),
            (None, None) => (default_namespace(), String::from("logos-k8s-smoke")),
        }
    }

    fn node_deployment_name(_release: &str, index: usize) -> String {
        format!("{K8S_FULLNAME_OVERRIDE}-node-{index}")
    }

    fn node_service_name(_release: &str, index: usize) -> String {
        format!("{K8S_FULLNAME_OVERRIDE}-node-{index}")
    }

    fn node_base_url(client: &Self::NodeClient) -> Option<String> {
        Some(client.base_url().to_string())
    }

    fn cfgsync_service(_release: &str) -> Option<(String, u16)> {
        Some((format!("{K8S_FULLNAME_OVERRIDE}-cfgsync"), cfgsync_port()))
    }

    fn build_cfgsync_override_artifacts(
        topology: &Self::Deployment,
        node_index: usize,
        hostnames: &[String],
        options: &testing_framework_core::scenario::StartNodeOptions<Self>,
    ) -> Result<Option<cfgsync_artifacts::ArtifactSet>, DynError> {
        build_node_artifact_override::<Self>(topology, node_index, hostnames, options)
            .map_err(Into::into)
    }
}

fn default_namespace() -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("logos-k8s-{stamp:x}-{:x}", process::id())
}

fn build_runtime_spec(
    topology: &DeploymentPlan,
    cfgsync_yaml: &str,
    artifacts_yaml: &str,
    cfgsync_file: std::path::PathBuf,
    scripts: assets::ScriptPaths,
    chart_path: std::path::PathBuf,
) -> testing_framework_runner_k8s::NodeRuntimeSpec {
    let layout = testing_framework_runner_k8s::RunnerAssetLayout::with_paths(
        LOGOS_RUNNER_MOUNT_PATH,
        "cfgsync.yaml",
        "cfgsync.artifacts.yaml",
        "scripts/run_cfgsync.sh",
        "scripts/run_logos.sh",
        "scripts/run_logos_node.sh",
    );

    let node_image = resolve_k8s_node_image();
    let bootstrap_image = resolve_k8s_bootstrap_image();
    let default_pull_policy = tf_env::nomos_testnet_image_pull_policy();
    let node_image_pull_policy = resolve_image_pull_policy(
        LOGOS_K8S_NODE_IMAGE_PULL_POLICY,
        &node_image,
        default_pull_policy.clone(),
    );
    let bootstrap_image_pull_policy = resolve_image_pull_policy(
        LOGOS_K8S_BOOTSTRAP_IMAGE_PULL_POLICY,
        &bootstrap_image,
        default_pull_policy,
    );

    testing_framework_runner_k8s::NodeRuntimeSpec {
        node_image: node_image.name,
        node_image_pull_policy,
        fullname_override: K8S_FULLNAME_OVERRIDE.to_string(),
        layout,
        nodes: build_node_group("node", topology.nodes()),
        shared_service: Some(
            SharedServiceSpec::new(
                "cfgsync".to_string(),
                bootstrap_image.name,
                bootstrap_image_pull_policy,
                cfgsync_port(),
                cfgsync_yaml.to_string(),
                artifacts_yaml.to_string(),
                cfgsync_file,
                scripts.run_cfgsync,
            )
            .with_env(BTreeMap::from([(
                "CFG_SERVER_STORAGE_PATH".to_string(),
                "/var/lib/cfgsync/deployment-settings.yaml".to_string(),
            )]))
            .with_writable_mount_path("/var/lib/cfgsync".to_string())
            .with_extra_files(vec![SharedServiceFileSpec::inline(
                "cfgsync.entropy".to_string(),
                "cfgsync.entropy".to_string(),
                "nomos-testing-framework-cfgsync-entropy".to_string(),
            )]),
        ),
        node_start_script_file: scripts.run_node,
        common_start_script_file: scripts.run_shared,
        chart_path,
        current_dir: assets::workspace_root().ok(),
    }
}

fn resolve_image_pull_policy(
    key: &str,
    image: &super::image::ResolvedImage,
    fallback: Option<String>,
) -> String {
    env::var(key)
        .ok()
        .or(fallback)
        .unwrap_or_else(|| match image.local {
            true => "Never".into(),
            false => "IfNotPresent".into(),
        })
}

fn build_node_group(kind: &'static str, nodes: &[NodePlan]) -> NodeGroup {
    let node_values = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| build_node_values(kind, index, node))
        .collect();

    NodeGroup::new(node_values)
}

fn build_node_values(kind: &'static str, index: usize, node: &NodePlan) -> NodeValues {
    let mut env = BTreeMap::new();
    env.insert("CFG_HOST_KIND".into(), kind.to_string());
    env.insert("CFG_HOST_IDENTIFIER".into(), format!("{kind}-{index}"));
    env.insert(
        "CFG_NETWORK_PORT".into(),
        node.general.network_config.backend.swarm.port.to_string(),
    );
    env.insert(
        "CFG_BLEND_PORT".into(),
        extract_udp_port(&node.general.blend_config.0.core.backend.listening_address).to_string(),
    );
    env.insert(
        "CFG_API_PORT".into(),
        node.general.api_config.address.port().to_string(),
    );

    NodeValues::new(
        vec![
            NodePortValues::tcp("http", node.general.api_config.address.port()),
            NodePortValues::tcp(
                "testing-http",
                node.general.api_config.testing_http_address.port(),
            ),
            NodePortValues::udp("swarm-udp", node.general.network_config.backend.swarm.port),
            NodePortValues::udp(
                "blend-udp",
                extract_udp_port(&node.general.blend_config.0.core.backend.listening_address),
            ),
        ],
        env,
    )
}

fn extract_udp_port(address: &lb_libp2p::Multiaddr) -> u16 {
    address
        .iter()
        .find_map(|protocol| match protocol {
            Protocol::Udp(port) => Some(port),
            _ => None,
        })
        .expect("blend multiaddr should contain a UDP port")
}
