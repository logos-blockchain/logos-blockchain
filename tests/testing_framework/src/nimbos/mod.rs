//! Running Nimbos through the testing framework (TF).

mod deployment;

use std::{
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use lb_libp2p::{Multiaddr, PeerId, Protocol, identity::Keypair};
use reqwest::Url;
use serde::Serialize;
use testing_framework_core::{
    scenario::{Application, DynError, NodeAccess, PeerSelection, ReadinessProbe},
    topology::DeploymentDescriptor,
};
use testing_framework_runner_local::{
    LaunchFile, LaunchSpec, LocalBuildContext, LocalDeployerEnv, PreparedNode,
};

use crate::SharedDeployment;

const CONFIG_FILE: &str = "nimbos.toml";
const DEPLOYMENT_FILE: &str = "deployment.yaml";
const NETWORK_KEY_FILE: &str = "network.key";

/// Inputs passed to TF when deploying a Nimbos cluster.
///
/// Holds shared network settings, binary and circuits paths, and one network
/// key per node. Those keys determine the peer IDs used in bootstrap addresses.
#[derive(Clone)]
pub struct NimbosDeployment {
    keys: Vec<Keypair>,
    binary: PathBuf,
    circuits_dir: PathBuf,
    deployment_yaml: Arc<str>,
    bootstrap_peers: Vec<Multiaddr>,
}

impl NimbosDeployment {
    /// Read native Nimbos deployment YAML using caller-supplied binary paths.
    ///
    /// The file is preserved verbatim; loading it does not validate protocol
    /// or genesis compatibility with the binary.
    pub fn new(
        binary: &Path,
        circuits_dir: &Path,
        deployment_file: &Path,
    ) -> Result<Self, DynError> {
        Self::from_deployment_yaml(binary, circuits_dir, fs::read_to_string(deployment_file)?)
    }

    pub fn from_deployment_yaml(
        binary: &Path,
        circuits_dir: &Path,
        deployment_yaml: String,
    ) -> Result<Self, DynError> {
        Ok(Self {
            keys: vec![Keypair::generate_ed25519()],
            binary: binary.canonicalize()?,
            circuits_dir: circuits_dir.canonicalize()?,
            deployment_yaml: deployment_yaml.into(),
            bootstrap_peers: Vec::new(),
        })
    }

    #[must_use]
    pub fn with_node_count(mut self, count: NonZeroUsize) -> Self {
        self.keys
            .resize_with(count.get(), Keypair::generate_ed25519);
        self
    }

    pub fn peer_id(&self, index: usize) -> Result<PeerId, DynError> {
        self.keys
            .get(index)
            .map(|key| key.public().to_peer_id())
            .ok_or_else(|| {
                format!(
                    "Nimbos node index {index} exceeds capacity {}",
                    self.keys.len()
                )
                .into()
            })
    }

    /// Nimbos bootstrap addresses require both a QUIC endpoint and a peer ID.
    ///
    /// A mixed app supplies the live Logos addresses here to connect the two
    /// child clusters. The mixed example lives in `logos-blockchain-tests`.
    pub fn with_bootstrap_peers(mut self, peers: Vec<Multiaddr>) -> Result<Self, DynError> {
        for peer in &peers {
            validate_peer_address(peer)?;
        }

        self.bootstrap_peers = peers;
        Ok(self)
    }
}

fn validate_peer_address(peer: &Multiaddr) -> Result<(), DynError> {
    if !peer.iter().any(|part| matches!(part, Protocol::Udp(_)))
        || !peer.iter().any(|part| matches!(part, Protocol::QuicV1))
        || !matches!(peer.iter().last(), Some(Protocol::P2p(_)))
    {
        return Err(format!(
            "Nimbos bootstrap peer must end in /p2p/<peer-id> and use UDP/QUIC: {peer}"
        )
        .into());
    }
    Ok(())
}

impl DeploymentDescriptor for NimbosDeployment {
    fn node_count(&self) -> usize {
        self.keys.len()
    }
}

/// Per-node settings serialized into `nimbos.toml`.
///
/// The deployment YAML and network key are written as separate launch files.
#[derive(Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct NimbosConfig {
    #[serde(skip)]
    deployment: NimbosDeployment,
    #[serde(skip)]
    network_key: Vec<u8>,
    netkey_file: &'static str,
    log_file: PathBuf,
    rest_port: u16,
    quic_port: u16,
    listen_address: &'static str,
    network: &'static str,
    bootstrap_node: Vec<String>,
    deployment_settings: &'static str,
    circuits_dir: PathBuf,
    num_threads: usize,
    data_dir: PathBuf,
}

/// TF adapter for the Nimbos binary, configuration files and readiness probe.
///
/// [`Application`] defines the types and readiness policy; [`LocalDeployerEnv`]
/// supplies per-node configuration and launch instructions. TF owns processes,
/// ports, node directories and cleanup.
pub struct NimbosEnv;

impl NimbosEnv {
    /// Translate shared genesis and network settings into Nimbos deployment
    /// YAML.
    ///
    /// This is called by the application, before TF's per-node configuration
    /// hooks. The cluster uses the generated node count and its own network
    /// keys.
    pub fn prepare_deployment(
        shared: &SharedDeployment,
        binary: &Path,
        circuits_dir: &Path,
    ) -> Result<NimbosDeployment, DynError> {
        let count = NonZeroUsize::new(shared.node_count())
            .ok_or("Nimbos deployment requires at least one node")?;
        let yaml = deployment::from_logos_yaml(shared.deployment_yaml()?)?;
        Ok(
            NimbosDeployment::from_deployment_yaml(binary, circuits_dir, yaml)?
                .with_node_count(count),
        )
    }
}

#[async_trait]
impl Application for NimbosEnv {
    type Deployment = NimbosDeployment;
    // Do not require Logos HTTP response types just to launch a node.
    type NodeClient = Url;
    type NodeConfig = NimbosConfig;

    fn build_node_client(access: &NodeAccess) -> Result<Url, DynError> {
        access.api_base_url()
    }

    /// TCP readiness checks the REST listener; tests check peer connectivity
    /// and protocol behavior separately.
    fn node_readiness_probe() -> ReadinessProbe {
        ReadinessProbe::Tcp
    }
}

#[async_trait]
impl LocalDeployerEnv for NimbosEnv {
    /// TF supplies the node index, peer ports and allocator for extra ports.
    ///
    /// Combine those with this deployment's keys and bootstrap peers to build
    /// the node's native config. TF uses the returned name and network port
    /// when managing the cluster.
    fn build_node_config(
        context: LocalBuildContext<'_, Self>,
    ) -> Result<PreparedNode<NimbosConfig>, DynError> {
        let LocalBuildContext {
            topology: deployment,
            index,
            ports,
            options,
            template_config: template,
            peers,
        } = context;

        let key = deployment.keys.get(index).ok_or_else(|| {
            format!(
                "Nimbos node index {index} exceeds capacity {}",
                deployment.keys.len()
            )
        })?;
        let mut bootstrap = deployment
            .bootstrap_peers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let selected = match options.common.peers.as_ref() {
            Some(PeerSelection::None) => Vec::new(),
            Some(PeerSelection::Named(names)) => {
                let mut selected = Vec::new();
                for name in names {
                    if let Some(peer) = peers.iter().find(|peer| peer.name() == Some(name.as_str()))
                    {
                        selected.push(peer);
                    } else {
                        return Err(format!("unknown Nimbos peer '{name}'").into());
                    }
                }
                selected
            }
            None | Some(PeerSelection::DefaultLayout) => peers
                .iter()
                .filter(|peer| index != 0 && peer.index() == 0)
                .collect(),
        };
        for peer in selected {
            bootstrap.push(format!(
                "/ip4/127.0.0.1/udp/{}/quic-v1/p2p/{}",
                peer.network_port(),
                deployment.peer_id(peer.index())?
            ));
        }

        let mut config = template.cloned().unwrap_or_else(|| NimbosConfig {
            deployment: deployment.clone(),
            network_key: Vec::new(),
            netkey_file: NETWORK_KEY_FILE,
            log_file: PathBuf::from("nimbos.log"),
            rest_port: 0,
            quic_port: 0,
            listen_address: "127.0.0.1",
            network: "testnet",
            bootstrap_node: Vec::new(),
            deployment_settings: DEPLOYMENT_FILE,
            circuits_dir: deployment.circuits_dir.clone(),
            num_threads: 2,
            data_dir: PathBuf::from("data"),
        });
        config.network_key = key.to_protobuf_encoding()?;
        config.bootstrap_node = bootstrap;
        config.rest_port = ports.allocate("http")?;
        config.quic_port = ports.network_port();

        Ok(PreparedNode {
            name: format!("nimbos-{index}"),
            config,
            network_port: ports.network_port(),
        })
    }

    /// Describe the files and command TF needs to launch this node.
    ///
    /// TF writes the TOML, deployment YAML and network key into the node's
    /// directory before starting the supplied binary with `--config-file`.
    async fn build_launch_spec(
        config: &NimbosConfig,
        dir: &Path,
        _label: &str,
    ) -> Result<LaunchSpec, DynError> {
        let mut config = config.clone();
        config.data_dir = dir.join("data");
        config.log_file = dir.join("nimbos.log");

        Ok(LaunchSpec {
            binary: config.deployment.binary.clone(),
            files: vec![
                LaunchFile {
                    relative_path: CONFIG_FILE.into(),
                    contents: toml::to_string(&config)?.into_bytes(),
                },
                LaunchFile {
                    relative_path: DEPLOYMENT_FILE.into(),
                    contents: config.deployment.deployment_yaml.as_bytes().to_vec(),
                },
                LaunchFile {
                    relative_path: NETWORK_KEY_FILE.into(),
                    contents: config.network_key.clone(),
                },
            ],
            args: vec![format!("--config-file={CONFIG_FILE}")],
            env: Vec::new(),
        })
    }

    fn http_api_port(config: &NimbosConfig) -> Option<u16> {
        Some(config.rest_port)
    }
}
