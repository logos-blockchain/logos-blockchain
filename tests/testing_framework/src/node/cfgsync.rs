use std::net::{IpAddr, Ipv4Addr};

use cfgsync_artifacts::{ArtifactFile, ArtifactSet};
use lb_libp2p::{Multiaddr, PeerId, Protocol, ed25519};
use lb_node::config::{RunConfig, network::serde::nat::Config as NatConfig};
use testing_framework_core::{
    cfgsync::StaticNodeConfigProvider,
    scenario::{DynError, PeerSelection, StartNodeOptions},
};
use thiserror::Error;

use crate::{
    framework::{LbcEnv, local::build_node_run_config},
    node::DeploymentPlan,
};

#[derive(Debug, Error)]
#[error("{source}")]
pub struct NodeCfgsyncError {
    #[from]
    source: DynError,
}

impl StaticNodeConfigProvider for LbcEnv {
    type Error = NodeCfgsyncError;

    fn build_node_config(
        deployment: &DeploymentPlan,
        node_index: usize,
    ) -> Result<RunConfig, Self::Error> {
        let node = &deployment.nodes()[node_index];
        build_node_run_config(
            deployment,
            node,
            deployment.config().node_config_override(node.index()),
        )
        .map_err(Into::into)
    }

    fn rewrite_for_hostnames(
        deployment: &DeploymentPlan,
        node_index: usize,
        hostnames: &[String],
        config: &mut RunConfig,
    ) -> Result<(), Self::Error> {
        let rewritten_peers = rewrite_node_peers(deployment, node_index, hostnames)
            .map_err(NodeCfgsyncError::from)?;

        apply_launch_ready_bind_addresses(config);
        apply_runtime_networking(config, &hostnames[node_index], rewritten_peers);

        Ok(())
    }

    fn serialize_node_config(config: &RunConfig) -> Result<String, Self::Error> {
        serde_yaml::to_string(config).map_err(|source| NodeCfgsyncError {
            source: source.into(),
        })
    }

    fn build_node_artifacts_for_options(
        deployment: &DeploymentPlan,
        node_index: usize,
        hostnames: &[String],
        options: &StartNodeOptions<Self>,
    ) -> Result<Option<ArtifactSet>, Self::Error> {
        let mut config = Self::build_node_config(deployment, node_index)?;
        apply_launch_ready_bind_addresses(&mut config);

        match &options.peers {
            PeerSelection::DefaultLayout => {
                if options.config_override.is_none() && options.config_patch.is_none() {
                    return Ok(None);
                }
                let peers = rewrite_node_peers(deployment, node_index, hostnames)
                    .map_err(NodeCfgsyncError::from)?;
                apply_runtime_networking(&mut config, &hostnames[node_index], peers);
            }
            PeerSelection::Named(_) | PeerSelection::None => {
                let peers =
                    resolve_selected_peers(deployment, node_index, hostnames, &options.peers)
                        .map_err(NodeCfgsyncError::from)?;
                apply_runtime_networking(&mut config, &hostnames[node_index], peers);
            }
        }

        if let Some(override_config) = options.config_override.clone() {
            config = override_config;
            apply_launch_ready_bind_addresses(&mut config);
        }

        if let Some(config_patch) = &options.config_patch {
            config = config_patch(config).map_err(NodeCfgsyncError::from)?;
        }

        let yaml = Self::serialize_node_config(&config)?;
        Ok(Some(ArtifactSet::new(vec![ArtifactFile::new(
            "/config.yaml".to_string(),
            yaml,
        )])))
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

fn apply_runtime_networking(
    config: &mut RunConfig,
    hostname: &str,
    rewritten_peers: Vec<Multiaddr>,
) {
    let swarm_port = config.user.network.backend.swarm.port;
    let blend_port = multiaddr_port(&config.user.blend.core.backend.listening_address)
        .expect("blend listening address should contain a UDP port");

    config.user.network.backend.initial_peers = rewritten_peers;
    config.user.network.backend.swarm.nat = NatConfig::Static {
        external_address: compose_peer_addr(hostname, swarm_port, None),
    };

    config.user.blend.core.backend.listening_address = bind_addr(blend_port);
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

fn resolve_selected_peers(
    deployment: &DeploymentPlan,
    node_index: usize,
    hostnames: &[String],
    peer_selection: &PeerSelection,
) -> Result<Vec<Multiaddr>, DynError> {
    match peer_selection {
        PeerSelection::DefaultLayout => rewrite_node_peers(deployment, node_index, hostnames),
        PeerSelection::None => Ok(Vec::new()),
        PeerSelection::Named(names) => {
            resolve_named_peers(deployment, node_index, hostnames, names)
        }
    }
}

fn resolve_named_peers(
    deployment: &DeploymentPlan,
    node_index: usize,
    hostnames: &[String],
    names: &[String],
) -> Result<Vec<Multiaddr>, DynError> {
    let nodes = deployment.nodes();
    let node_peer_ids = nodes
        .iter()
        .map(|node| peer_id_from_id(node.id))
        .collect::<Result<Vec<_>, _>>()?;

    let mut peers = Vec::with_capacity(names.len());
    for name in names {
        let Some(peer_index) = parse_node_index(name) else {
            return Err(std::io::Error::other(format!("unknown peer name '{name}'")).into());
        };
        let Some(peer_node) = nodes.get(peer_index) else {
            return Err(
                std::io::Error::other(format!("peer index out of range for '{name}'")).into(),
            );
        };
        if peer_index == node_index {
            continue;
        }

        peers.push(compose_peer_addr(
            &hostnames[peer_index],
            peer_node.general.network_config.backend.swarm.port,
            Some(&node_peer_ids[peer_index]),
        ));
    }

    Ok(peers)
}

fn parse_node_index(name: &str) -> Option<usize> {
    name.strip_prefix("node-")?.parse().ok()
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

fn bind_addr(port: u16) -> Multiaddr {
    let mut addr = Multiaddr::empty();
    addr.push(Protocol::Ip4(Ipv4Addr::UNSPECIFIED));
    addr.push(Protocol::Udp(port));
    addr.push(Protocol::QuicV1);
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
