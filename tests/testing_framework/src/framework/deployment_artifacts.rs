use anyhow::Result;
use cfgsync_adapter::MaterializedArtifacts;
use cfgsync_artifacts::ArtifactFile;
use lb_core::{
    mantle::GenesisTx as _,
    sdp::{Locator, ServiceType},
};
use lb_libp2p::{Multiaddr, Protocol};
use lb_node::config::deployment::DeploymentSettings;
use thiserror::Error;

use crate::{
    internal::DeploymentPlan,
    node::{
        NodePlan,
        configs::{
            default_e2e_deployment_settings,
            node_configs::consensus::{ProviderInfo, create_genesis_tx_with_declarations},
        },
    },
};

#[derive(Debug, Error)]
pub(crate) enum ArtifactError {
    #[error("deployment plan is missing `genesis_tx`")]
    MissingGenesisTx,
    #[error("runtime hostname count ({hostnames}) does not match node count ({nodes})")]
    HostnameCountMismatch { hostnames: usize, nodes: usize },
    #[error("node {node_index} blend address is missing a UDP port")]
    MissingBlendPort { node_index: usize },
    #[error("failed to serialize deployment settings from node run config: {source}")]
    SerializeDeployment {
        #[source]
        source: serde_yaml::Error,
    },
}

pub(crate) fn add_shared_deployment_file(
    topology: &DeploymentPlan,
    hostnames: &[String],
    materialized: &mut MaterializedArtifacts,
) -> Result<()> {
    if has_shared_file_path(materialized, "/deployment.yaml") {
        return Ok(());
    }

    let deployment_yaml = deployment_yaml(topology, hostnames)?;

    let mut shared = materialized.shared().clone();
    shared.files.push(ArtifactFile::new(
        "/deployment.yaml".to_string(),
        deployment_yaml,
    ));

    *materialized = materialized.clone().with_shared(shared);

    Ok(())
}

fn has_shared_file_path(materialized: &MaterializedArtifacts, path: &str) -> bool {
    materialized
        .shared()
        .files
        .iter()
        .any(|file| file.path == path)
}

fn deployment_yaml(topology: &DeploymentPlan, hostnames: &[String]) -> Result<String> {
    let deployment = deployment_settings(topology, hostnames)?;

    serde_yaml::to_string(&deployment)
        .map_err(|source| ArtifactError::SerializeDeployment { source }.into())
}

fn deployment_settings(
    topology: &DeploymentPlan,
    hostnames: &[String],
) -> Result<DeploymentSettings> {
    let genesis_tx = topology
        .config()
        .genesis_tx
        .clone()
        .ok_or(ArtifactError::MissingGenesisTx)?;

    let providers = collect_runtime_blend_providers(topology.nodes(), hostnames)?;
    let transfer_op = genesis_tx.genesis_transfer().clone();
    let genesis_tx = create_genesis_tx_with_declarations(transfer_op, providers);

    Ok(default_e2e_deployment_settings(genesis_tx))
}

fn collect_runtime_blend_providers(
    nodes: &[NodePlan],
    hostnames: &[String],
) -> Result<Vec<ProviderInfo>> {
    if nodes.len() != hostnames.len() {
        return Err(ArtifactError::HostnameCountMismatch {
            hostnames: hostnames.len(),
            nodes: nodes.len(),
        }
        .into());
    }

    let mut providers = Vec::with_capacity(nodes.len());

    for (index, (node, hostname)) in nodes.iter().zip(hostnames.iter()).enumerate() {
        let port = blend_udp_port(node, index)?;
        let locator = runtime_blend_locator(hostname, port);
        let (_, provider_sk, zk_sk) = &node.general.blend_config;

        providers.push(ProviderInfo {
            service_type: ServiceType::BlendNetwork,
            provider_sk: provider_sk.clone(),
            zk_sk: zk_sk.clone(),
            locator,
            note: node.general.consensus_config.blend_note.clone(),
        });
    }

    Ok(providers)
}

fn blend_udp_port(node: &NodePlan, node_index: usize) -> Result<u16> {
    node.general
        .blend_config
        .0
        .core
        .backend
        .listening_address
        .iter()
        .find_map(|protocol| match protocol {
            Protocol::Udp(port) => Some(port),
            _ => None,
        })
        .ok_or(ArtifactError::MissingBlendPort { node_index }.into())
}

fn runtime_blend_locator(hostname: &str, port: u16) -> Locator {
    let mut multiaddr = Multiaddr::empty();
    multiaddr.push(Protocol::Dns4(hostname.to_owned().into()));
    multiaddr.push(Protocol::Udp(port));
    multiaddr.push(Protocol::QuicV1);

    Locator::new(multiaddr)
}
