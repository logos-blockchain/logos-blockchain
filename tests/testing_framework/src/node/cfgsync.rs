use std::net::{IpAddr, Ipv4Addr};

use lb_libp2p::{Multiaddr, PeerId, Protocol, ed25519};
use lb_node::config::{RunConfig, network::serde::nat::Config as NatConfig};
use testing_framework_core::{cfgsync::CfgsyncEnv, scenario::DynError};
use thiserror::Error;

use crate::{
    framework::{LbcEnv, local::build_node_run_config},
    node::{DeploymentPlan, NodePlan},
};

#[derive(Debug, Error)]
#[error("{source}")]
pub struct NodeCfgsyncError {
    #[from]
    source: DynError,
}

impl CfgsyncEnv for LbcEnv {
    type Deployment = DeploymentPlan;
    type Node = NodePlan;
    type NodeConfig = RunConfig;
    type Error = NodeCfgsyncError;

    fn nodes(deployment: &Self::Deployment) -> &[Self::Node] {
        deployment.nodes()
    }

    fn node_identifier(index: usize, _node: &Self::Node) -> String {
        format!("node-{index}")
    }

    fn build_node_config(
        deployment: &Self::Deployment,
        node: &Self::Node,
    ) -> Result<Self::NodeConfig, Self::Error> {
        build_node_run_config(
            deployment,
            node,
            deployment.config().node_config_override(node.index()),
        )
        .map_err(Into::into)
    }

    fn rewrite_for_hostnames(
        deployment: &Self::Deployment,
        node_index: usize,
        hostnames: &[String],
        config: &mut Self::NodeConfig,
    ) -> Result<(), Self::Error> {
        let rewritten_peers = rewrite_node_peers(deployment, node_index, hostnames)
            .map_err(NodeCfgsyncError::from)?;

        apply_launch_ready_bind_addresses(config);
        apply_host_rewritten_networking(config, &hostnames[node_index], rewritten_peers);

        Ok(())
    }

    fn serialize_node_config(config: &Self::NodeConfig) -> Result<String, Self::Error> {
        serde_yaml::to_string(config).map_err(|source| NodeCfgsyncError {
            source: source.into(),
        })
    }
}

const fn apply_launch_ready_bind_addresses(config: &mut RunConfig) {
    config
        .user
        .api
        .backend
        .listen_address
        .set_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    config
        .user
        .api
        .testing
        .listen_address
        .set_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
}

fn apply_host_rewritten_networking(
    config: &mut RunConfig,
    hostname: &str,
    rewritten_peers: Vec<Multiaddr>,
) {
    let swarm_port = config.user.network.backend.swarm.port;
    config.user.network.backend.initial_peers = rewritten_peers;
    config.user.network.backend.swarm.nat = NatConfig::Static {
        external_address: compose_peer_addr(hostname, swarm_port, None),
    };
}

fn rewrite_node_peers(
    deployment: &DeploymentPlan,
    node_index: usize,
    hostnames: &[String],
) -> Result<Vec<Multiaddr>, DynError> {
    let nodes = deployment.nodes();
    let templates = nodes[node_index]
        .general
        .network_config
        .backend
        .initial_peers
        .clone();

    let node_peer_ids = nodes
        .iter()
        .map(|node| peer_id_from_id(node.id))
        .collect::<Result<Vec<_>, _>>()?;

    let original_ports = nodes
        .iter()
        .map(|node| node.general.network_config.backend.swarm.port)
        .collect::<Vec<_>>();

    let mut rewritten = Vec::new();
    for addr in templates {
        let Some(port) = multiaddr_port(&addr) else {
            continue;
        };

        let Some(peer_idx) = original_ports.iter().position(|value| *value == port) else {
            continue;
        };

        if peer_idx == node_index {
            continue;
        }

        rewritten.push(compose_peer_addr(
            &hostnames[peer_idx],
            port,
            Some(&node_peer_ids[peer_idx]),
        ));
    }

    Ok(rewritten)
}

fn compose_peer_addr(hostname: &str, port: u16, peer_id: Option<&PeerId>) -> Multiaddr {
    let mut addr = Multiaddr::empty();
    addr.push(Protocol::Dns4(hostname.to_owned().into()));
    addr.push(Protocol::Udp(port));
    addr.push(Protocol::QuicV1);

    if let Some(peer_id) = peer_id {
        addr.push(Protocol::P2p(*peer_id));
    }

    addr
}

fn peer_id_from_id(id: [u8; 32]) -> Result<PeerId, DynError> {
    let mut node_key_bytes = id;
    let node_key = ed25519::SecretKey::try_from_bytes(&mut node_key_bytes)
        .map_err(|_| std::io::Error::other("failed to decode node key for peer id"))?;

    Ok(PeerId::from_public_key(
        &ed25519::Keypair::from(node_key).public().into(),
    ))
}

fn multiaddr_port(addr: &Multiaddr) -> Option<u16> {
    addr.iter().find_map(|protocol| match protocol {
        Protocol::Udp(port) | Protocol::Tcp(port) => Some(port),
        _ => None,
    })
}
