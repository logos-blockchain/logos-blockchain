//! Runs saved native configuration through TF's ordinary local lifecycle.
//!
//! The selected node binary reads its own configuration schema. TF only
//! supplies runtime addresses and paths through the node CLI. It checks the few
//! native settings needed for an isolated test run, but never deserializes
//! genesis using the current checkout's types.

use std::{
    collections::{HashMap, HashSet},
    env,
    net::{Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, bail};
use async_trait::async_trait;
use lb_testing_framework::{
    LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL, LbcEnv, NodeHttpClient,
    local::{node_launch_spec, release_binary_provider},
};
use libp2p::PeerId;
use serde_yaml::Value;
use testing_framework_app::ClusterApp;
use testing_framework_core::{
    scenario::{Application, DynError, NodeAccess, PeerSelection, ReadinessProbe},
    topology::DeploymentDescriptor,
};
use testing_framework_runner_local::{
    BinaryProviderRef, LaunchSpec, LocalBuildContext, LocalDeployerEnv, NodeEndpointPort,
    NodeEndpoints, PathBinaryProvider, PreparedNode,
};
use tokio::{process::Command, time::timeout};

use super::SavedDeployment;

// A saved deployment must never silently fall back to building this checkout.
async fn selected_binary() -> Result<PathBuf, DynError> {
    binary_provider(
        env::var_os("LOGOS_BLOCKCHAIN_NODE_BIN").map(PathBuf::from),
        env::var_os(LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL).is_some_and(|url| !url.is_empty()),
    )?
    .resolve()
    .await
    .map_err(Into::into)
}

fn binary_provider(path: Option<PathBuf>, download: bool) -> Result<BinaryProviderRef, DynError> {
    if let Some(path) = path {
        if !path.is_file() {
            return Err(format!(
                "LOGOS_BLOCKCHAIN_NODE_BIN does not point to a file: {}",
                path.display()
            )
            .into());
        }

        return Ok(Arc::new(PathBinaryProvider::new(path.canonicalize()?)));
    }

    if download {
        return Ok(Arc::new(release_binary_provider()));
    }

    Err("set LOGOS_BLOCKCHAIN_NODE_BIN or LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL for saved configuration".into())
}

async fn read_peer_id(binary: &Path, config: &Path) -> anyhow::Result<PeerId> {
    let output = timeout(
        Duration::from_secs(30),
        Command::new(binary)
            .arg("get-peer-id")
            .arg("--config")
            .arg(config)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .with_context(|| format!("get-peer-id timed out for {}", config.display()))?
    .with_context(|| format!("cannot execute {} get-peer-id", binary.display()))?;

    if !output.status.success() {
        bail!(
            "{} could not read the peer ID from {}: {}",
            binary.display(),
            config.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout)?
        .trim()
        .parse()
        .with_context(|| format!("invalid get-peer-id output for {}", config.display()))
}

/// Native Logos deployment paired with the binary that will launch it.
#[derive(Clone)]
pub struct SavedLogosDeployment {
    binary: PathBuf,
    saved: SavedDeployment,
}

impl DeploymentDescriptor for SavedLogosDeployment {
    fn node_count(&self) -> usize {
        self.saved.shared_deployment().node_count()
    }
}

/// Local Logos adapter for native configuration saved alongside a matching
/// binary.
#[derive(Clone)]
pub struct SavedLogosEnv;

impl SavedLogosEnv {
    /// Selects the Logos binary, checks the saved configuration and identities,
    /// and returns a TF app ready for deployment.
    pub async fn prepare_app(saved: SavedDeployment) -> Result<ClusterApp<Self>, DynError> {
        let binary = selected_binary().await?;
        for (index, node) in saved.nodes.iter().enumerate() {
            validate_runtime_settings(&node.yaml).with_context(|| {
                format!("unsupported saved Logos config {}", node.path.display())
            })?;
            let expected = saved
                .shared_deployment()
                .network_key(index)?
                .public()
                .to_peer_id();
            let actual = read_peer_id(&binary, &node.path).await?;
            if actual != expected {
                return Err(format!(
                    "selected binary reports a different peer ID for {} than its saved network key",
                    node.path.display()
                )
                .into());
            }
        }

        Ok(ClusterApp::new(SavedLogosDeployment { binary, saved }))
    }
}

/// Native files together with the runtime addresses allocated by TF.
#[derive(Clone)]
pub struct SavedLogosNodeConfig {
    binary: PathBuf,
    user_yaml: String,
    deployment_yaml: String,
    network_port: u16,
    http_port: u16,
    blend_port: u16,
    peers: Vec<String>,
}

#[async_trait]
impl Application for SavedLogosEnv {
    type Deployment = SavedLogosDeployment;
    type NodeConfig = SavedLogosNodeConfig;
    type NodeClient = NodeHttpClient;

    fn build_node_client(access: &NodeAccess) -> Result<Self::NodeClient, DynError> {
        LbcEnv::build_node_client(access)
    }

    fn node_readiness_probe() -> ReadinessProbe {
        LbcEnv::node_readiness_probe()
    }
}

#[async_trait]
impl LocalDeployerEnv for SavedLogosEnv {
    fn build_node_config(
        context: LocalBuildContext<'_, Self>,
    ) -> Result<PreparedNode<Self::NodeConfig>, DynError> {
        let topology = context.topology;
        let node =
            topology.saved.nodes.get(context.index).ok_or(
                "the scenario requested more nodes than the prepared configuration provides",
            )?;
        let selection = context.options.common.peers.as_ref();
        let mut unresolved = match selection {
            Some(PeerSelection::Named(names)) => names.iter().cloned().collect::<HashSet<_>>(),
            _ => HashSet::new(),
        };
        let peers = context
            .peers
            .iter()
            .filter(|peer| match selection {
                Some(PeerSelection::None) => false,
                Some(PeerSelection::Named(_)) => {
                    peer.name().is_some_and(|name| unresolved.remove(name))
                }
                _ => true,
            })
            .map(|peer| {
                Ok(format!(
                    "/ip4/127.0.0.1/udp/{}/quic-v1/p2p/{}",
                    peer.network_port(),
                    topology
                        .saved
                        .shared_deployment()
                        .network_key(peer.index())?
                        .public()
                        .to_peer_id()
                ))
            })
            .collect::<Result<Vec<_>, DynError>>()?;

        if !unresolved.is_empty() {
            return Err(format!("unknown prepared peers: {unresolved:?}").into());
        }

        let network_port = context.ports.network_port();
        Ok(PreparedNode {
            name: format!("node-{}", context.index),
            network_port,
            config: SavedLogosNodeConfig {
                binary: topology.binary.clone(),
                user_yaml: node.yaml.clone(),
                deployment_yaml: topology
                    .saved
                    .shared_deployment()
                    .deployment_yaml()?
                    .to_owned(),
                network_port,
                http_port: context.ports.allocate("http")?,
                blend_port: context.ports.allocate("blend")?,
                peers,
            },
        })
    }

    async fn build_launch_spec(
        config: &Self::NodeConfig,
        dir: &Path,
        label: &str,
    ) -> Result<LaunchSpec, DynError> {
        let mut spec = node_launch_spec(
            config.binary.clone(),
            dir,
            config.user_yaml.clone(),
            config.deployment_yaml.clone(),
        );

        // Native files must have no initial peers. The scenario supplies the
        // selected peer set, including an empty set, for each fresh launch.
        spec.args.extend([
            "--net-host".into(),
            "127.0.0.1".into(),
            "--net-port".into(),
            config.network_port.to_string(),
            "--external-address".into(),
            format!("/ip4/127.0.0.1/udp/{}/quic-v1", config.network_port),
            "--http-host".into(),
            format!("127.0.0.1:{}", config.http_port),
            "--blend-addr".into(),
            format!("/ip4/127.0.0.1/udp/{}/quic-v1", config.blend_port),
            "--state-path".into(),
            dir.display().to_string(),
            "--log-backend".into(),
            "file".into(),
            "--log-dir".into(),
            dir.display().to_string(),
            "--log-path".into(),
            format!("__logs-{label}.log"),
            "--skip-ibd".into(),
        ]);
        if !config.peers.is_empty() {
            spec.args
                .extend(["--net-initial-peers".into(), config.peers.join(",")]);
        }

        Ok(spec)
    }

    fn node_endpoints(config: &Self::NodeConfig) -> Result<NodeEndpoints, DynError> {
        let mut endpoints = NodeEndpoints {
            api: SocketAddr::from((Ipv4Addr::LOCALHOST, config.http_port)),
            extra_ports: HashMap::new(),
        };
        endpoints.insert_port(NodeEndpointPort::Network, config.network_port);
        Ok(endpoints)
    }
}

// These runtime fields are shared by the supported Logos CLI versions. Checking
// them does not require linking the old version's full configuration schema.
fn validate_runtime_settings(yaml: &str) -> anyhow::Result<()> {
    let config: Value = serde_yaml::from_str(yaml)?;
    let peer_settings = [
        (
            "network.backend.initial_peers",
            &config["network"]["backend"]["initial_peers"],
        ),
        (
            "cryptarchia.network.bootstrap.ibd.peers",
            &config["cryptarchia"]["network"]["bootstrap"]["ibd"]["peers"],
        ),
    ];
    for (setting, value) in peer_settings {
        let peers = value
            .as_sequence()
            .with_context(|| format!("{setting} must be an empty list"))?;
        if !peers.is_empty() {
            bail!("{setting} must be empty; the scenario selects peers");
        }
    }

    let folder = config["storage"]["backend"]["folder_name"]
        .as_str()
        .context("storage.backend.folder_name must be a relative directory")?;
    if folder.is_empty()
        || Path::new(folder)
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        bail!("storage.backend.folder_name must stay inside the node runtime directory");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{binary_provider, validate_runtime_settings};

    #[test]
    fn saved_runs_require_an_explicit_binary() {
        assert!(binary_provider(None, false).is_err());
        let dir = tempfile::tempdir().unwrap();
        for path in [
            dir.path().join("missing"),
            dir.path().to_owned(),
            PathBuf::new(),
        ] {
            assert!(binary_provider(Some(path), true).is_err());
        }
    }

    #[tokio::test]
    async fn explicit_binary_takes_precedence_over_download() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("node");
        fs::write(&binary, "selected binary").unwrap();
        let provider = binary_provider(Some(binary.clone()), true).unwrap();
        assert_eq!(
            provider.resolve().await.unwrap(),
            binary.canonicalize().unwrap()
        );
    }

    const NODE_CONFIG: &str = include_str!("../fixtures/logos-0.3.0-rc.5/node-1.yaml");

    #[test]
    fn saved_core_fixture_satisfies_runtime_requirements() {
        validate_runtime_settings(NODE_CONFIG).expect("valid native fixture");
    }

    #[test]
    fn saved_peers_cannot_override_scenario_topology() {
        let config = NODE_CONFIG.replace(
            "initial_peers: []",
            "initial_peers: [/ip4/127.0.0.1/udp/1234/quic-v1]",
        );

        let error = validate_runtime_settings(&config).expect_err("saved peer must be rejected");
        assert!(error.to_string().contains("the scenario selects peers"));
    }

    #[test]
    fn saved_database_must_stay_in_the_node_runtime_directory() {
        for folder in ["/tmp/shared-db", "../shared-db"] {
            let config = NODE_CONFIG.replace("folder_name: db", &format!("folder_name: {folder}"));
            let error = validate_runtime_settings(&config).expect_err("database would escape");
            assert!(error.to_string().contains("node runtime directory"));
        }
    }
}
