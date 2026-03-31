use std::{
    collections::HashMap,
    env, fs, io,
    net::{Ipv4Addr, UdpSocket},
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use config::{api, sdp, state, storage, wallet};
use lb_core::mantle::{self, genesis_tx::GenesisTx};
use lb_key_management_system_service::keys::{Key, secured_key::SecuredKey as _};
use lb_libp2p::Multiaddr;
use lb_node::{
    UserConfig, config,
    config::{
        RunConfig,
        tracing::serde::{Level, logger},
    },
};
use rand::Rng as _;
use testing_framework_core::scenario::{Application, DynError, PeerSelection, StartNodeOptions};
use testing_framework_runner_local::{
    BinaryConfig, BinaryResolver, BuiltNodeConfig, LaunchEnvVar, LaunchFile, LocalDeployerEnv,
    NodeConfigEntry, NodeEndpointPort, NodeEndpoints, ProcessSpawnError, env::Node,
    process::LaunchSpec,
};
use tracing::debug;

use crate::{
    LOGOS_BLOCKCHAIN_LOG_LEVEL, env as tf_env,
    framework::LbcEnv,
    node::{
        DeploymentPlan, NodeHttpClient, NodePlan,
        configs::{
            Config, Libp2pNetworkLayout, NetworkParams, create_node_config_for_node,
            default_e2e_deployment_settings, deployment::TopologyConfig,
            key_id_for_preload_backend,
        },
    },
};

const LOGS_PREFIX: &str = "__logs";
const DEFAULT_BLEND_NETWORK_PORT: u16 = 3400;
/// The default filename for the user config.
pub const USER_CONFIG_FILE: &str = "node.yaml";
/// The default filename for the deployment config.
pub const DEPLOYMENT_CONFIG_FILE: &str = "deployment.yaml";

struct PlannedLocalNodeConfig {
    config: Config,
    descriptor_override: Option<RunConfig>,
    genesis_tx: GenesisTx,
    port_strategy: PortStrategy,
}

#[derive(Clone, Copy)]
enum PortStrategy {
    PreservePlannedPorts,
    AllocateEphemeralPorts,
}

#[async_trait]
impl LocalDeployerEnv for LbcEnv {
    fn build_node_config(
        topology: &Self::Deployment,
        index: usize,
        peer_ports_by_name: &HashMap<String, u16>,
        options: &StartNodeOptions<Self>,
        peer_ports: &[u16],
    ) -> Result<BuiltNodeConfig<<Self as Application>::NodeConfig>, DynError> {
        build_dynamic_node_config(
            topology,
            index,
            peer_ports_by_name,
            options,
            peer_ports,
            None,
        )
    }

    fn build_node_config_from_template(
        topology: &Self::Deployment,
        index: usize,
        peer_ports_by_name: &HashMap<String, u16>,
        options: &StartNodeOptions<Self>,
        peer_ports: &[u16],
        template_config: Option<&<Self as Application>::NodeConfig>,
    ) -> Result<BuiltNodeConfig<<Self as Application>::NodeConfig>, DynError> {
        build_dynamic_node_config(
            topology,
            index,
            peer_ports_by_name,
            options,
            peer_ports,
            template_config,
        )
    }

    fn build_initial_node_configs(
        topology: &Self::Deployment,
    ) -> Result<Vec<NodeConfigEntry<<Self as Application>::NodeConfig>>, ProcessSpawnError> {
        topology
            .nodes()
            .iter()
            .map(|node| {
                let label = format!("node-{}", node.index());
                let config = build_node_run_config(
                    topology,
                    node,
                    topology.config().node_config_override(node.index()),
                )
                .map_err(|source| ProcessSpawnError::Config { source })?;
                Ok::<_, ProcessSpawnError>(NodeConfigEntry {
                    name: label,
                    config,
                })
            })
            .collect::<Result<Vec<_>, _>>()
    }

    fn initial_persist_dir(
        topology: &Self::Deployment,
        node_name: &str,
        _index: usize,
    ) -> Option<PathBuf> {
        Some(topology.config().scenario_base_dir.join(node_name))
    }

    fn build_launch_spec(
        config: &<Self as Application>::NodeConfig,
        dir: &Path,
        label: &str,
    ) -> Result<LaunchSpec, DynError> {
        let mut config = config.clone();
        ensure_recovery_paths(dir).map_err(|source| -> DynError { source.into() })?;
        config.user.tracing.level = configured_node_log_level();

        if !tf_env::debug_tracing() {
            let log_prefix = format!("{LOGS_PREFIX}-{label}");
            config.user.tracing.logger = configure_logging(dir, &log_prefix);
        }

        config.user.state.base_folder = dir.to_path_buf();
        "db".clone_into(&mut config.user.storage.backend.folder_name);

        let user_yaml = serde_yaml::to_string(&config.user).map_err(io::Error::other)?;
        let deployment_yaml =
            serde_yaml::to_string(&config.deployment).map_err(io::Error::other)?;

        build_node_launch_spec(dir, user_yaml, deployment_yaml)
    }

    fn node_endpoints(config: &<Self as Application>::NodeConfig) -> NodeEndpoints {
        let mut endpoints = NodeEndpoints {
            api: config.user.api.backend.listen_address,
            ..Default::default()
        };

        add_endpoint_ports(&mut endpoints, config);

        endpoints
    }

    fn node_peer_port(node: &Node<Self>) -> u16 {
        node.endpoints()
            .port(&NodeEndpointPort::Network)
            .unwrap_or_else(|| node.config().user.network.backend.swarm.port)
    }

    fn node_client(endpoints: &NodeEndpoints) -> Self::NodeClient {
        let testing_api = endpoints
            .port(&NodeEndpointPort::TestingApi)
            .map(|port| (endpoints.api.ip(), port).into());

        NodeHttpClient::new(endpoints.api, testing_api)
    }

    fn readiness_endpoint_path() -> &'static str {
        "/cryptarchia/info"
    }

    async fn wait_readiness_stable(nodes: &[Node<Self>]) -> Result<(), DynError> {
        super::readiness::wait_readiness_stable(nodes).await
    }
}

fn ensure_recovery_paths(base_dir: &Path) -> io::Result<()> {
    let recovery_dir = base_dir.join("recovery");
    fs::create_dir_all(&recovery_dir)?;

    let mempool_path = recovery_dir.join("mempool.json");
    if !mempool_path.exists() {
        fs::write(&mempool_path, "{}")?;
    }

    let blend_core_path = recovery_dir.join("blend").join("core.json");
    if let Some(parent) = blend_core_path.parent() {
        fs::create_dir_all(parent)?;
    }

    if !blend_core_path.exists() {
        fs::write(&blend_core_path, "{}")?;
    }

    Ok(())
}

fn add_endpoint_ports(endpoints: &mut NodeEndpoints, config: &RunConfig) {
    endpoints.insert_port(
        NodeEndpointPort::TestingApi,
        config.user.api.testing.listen_address.port(),
    );
    endpoints.insert_port(
        NodeEndpointPort::Network,
        config.user.network.backend.swarm.port,
    );
}

fn allocate_udp_port(label: &'static str) -> Result<u16, DynError> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|source| {
        io::Error::other(format!("failed to allocate {label} UDP port: {source}"))
    })?;

    socket
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|source| {
            io::Error::other(format!("failed to read {label} UDP port: {source}")).into()
        })
}

fn build_node_launch_spec(
    dir: &Path,
    user_yaml: String,
    deployment_yaml: String,
) -> Result<LaunchSpec, DynError> {
    let config_path = dir.join(USER_CONFIG_FILE);
    let deployment_path = dir.join(DEPLOYMENT_CONFIG_FILE);
    let time_backend =
        env::var("LOGOS_BLOCKCHAIN_TIME_BACKEND").unwrap_or_else(|_| "monotonic".to_owned());

    Ok(LaunchSpec {
        binary: BinaryResolver::resolve_path(&node_binary_config()),
        files: vec![
            launch_file(USER_CONFIG_FILE, user_yaml.into_bytes()),
            launch_file(DEPLOYMENT_CONFIG_FILE, deployment_yaml.into_bytes()),
        ],
        args: vec![
            config_path.to_string_lossy().to_string(),
            "--deployment".to_owned(),
            deployment_path.to_string_lossy().to_string(),
        ],
        env: vec![LaunchEnvVar::new(
            "LOGOS_BLOCKCHAIN_TIME_BACKEND",
            time_backend,
        )],
    })
}

fn launch_file(relative_path: &str, contents: Vec<u8>) -> LaunchFile {
    LaunchFile {
        relative_path: PathBuf::from(relative_path),
        contents,
    }
}

const fn node_binary_config() -> BinaryConfig {
    BinaryConfig {
        env_var: "LOGOS_BLOCKCHAIN_NODE_BIN",
        binary_name: "logos-blockchain-node",
        fallback_path: "target/debug/logos-blockchain-node",
    }
}

fn configure_logging(base_dir: &Path, prefix: &str) -> logger::Layers {
    debug!(prefix, base_dir = %base_dir.display(), "configuring node logging");

    if let Some(log_dir) = tf_env::nomos_log_dir() {
        match fs::create_dir_all(&log_dir) {
            Ok(()) => {
                return logger::Layers {
                    file: Some(logger::FileConfig {
                        directory: log_dir,
                        prefix: Some(prefix.into()),
                    }),
                    loki: None,
                    gelf: None,
                    otlp: None,
                    stdout: false,
                    stderr: false,
                };
            }

            Err(error) => {
                tracing::warn!(
                    %error,
                    "failed to create LOGOS_BLOCKCHAIN_LOG_DIR; falling back to node dir"
                );
            }
        }
    }

    logger::Layers {
        file: Some(logger::FileConfig {
            directory: base_dir.to_owned(),
            prefix: Some(prefix.into()),
        }),
        loki: None,
        gelf: None,
        otlp: None,
        stdout: false,
        stderr: false,
    }
}

fn configured_node_log_level() -> Level {
    env::var(LOGOS_BLOCKCHAIN_LOG_LEVEL)
        .ok()
        .and_then(|raw| raw.parse::<Level>().ok())
        .unwrap_or(Level::INFO)
}

fn build_dynamic_node_config(
    topology: &DeploymentPlan,
    index: usize,
    peer_ports_by_name: &HashMap<String, u16>,
    options: &StartNodeOptions<LbcEnv>,
    peer_ports: &[u16],
    template_config: Option<&RunConfig>,
) -> Result<BuiltNodeConfig<RunConfig>, DynError> {
    let plan = plan_local_node_config(
        topology,
        index,
        peer_ports_by_name,
        &options.peers,
        peer_ports,
    )?;
    let mut config =
        finalize_dynamic_run_config(&plan, options.config_override.as_ref(), template_config);
    let mut network_port = config.user.network.backend.swarm.port;

    match plan.port_strategy {
        PortStrategy::PreservePlannedPorts => {}
        PortStrategy::AllocateEphemeralPorts => {
            network_port = allocate_udp_port("network")?;
            let blend_port = allocate_udp_port("blend")?;
            config.user.network.backend.swarm.port = network_port;
            config.user.blend.core.backend.listening_address =
                lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, blend_port);
        }
    }

    Ok(BuiltNodeConfig {
        config,
        network_port,
    })
}

fn plan_local_node_config(
    descriptors: &DeploymentPlan,
    index: usize,
    peer_ports_by_name: &HashMap<String, u16>,
    peer_selection: &PeerSelection,
    peer_ports: &[u16],
) -> Result<PlannedLocalNodeConfig, DynError> {
    let base_node = descriptors
        .nodes()
        .first()
        .ok_or_else(|| io::Error::other("generated topology must include at least one node"))?;

    let base_consensus = &base_node.general.consensus_config;
    let base_time = &base_node.general.time_config;

    if let Some(node) = descriptors.nodes().get(index) {
        let mut config = node.general.clone();
        let initial_peers = resolve_initial_peers(
            peer_ports_by_name,
            peer_selection,
            &config.network_config.backend.initial_peers,
            descriptors,
            peer_ports,
        )?;

        config.network_config.backend.initial_peers = initial_peers;

        return Ok(PlannedLocalNodeConfig {
            config,
            descriptor_override: descriptors.config().node_config_override(index).cloned(),
            genesis_tx: descriptors
                .config()
                .genesis_tx
                .clone()
                .ok_or_else(|| io::Error::other("missing topology genesis tx"))?,
            port_strategy: PortStrategy::PreservePlannedPorts,
        });
    }

    let id = {
        let mut id = [0u8; 32];
        rand::thread_rng().fill(&mut id);
        id
    };

    let network_port = base_node.general.network_config.backend.swarm.port;
    let blend_port = DEFAULT_BLEND_NETWORK_PORT;
    let initial_peers = resolve_initial_peers(
        peer_ports_by_name,
        peer_selection,
        &[],
        descriptors,
        peer_ports,
    )?;

    let config = {
        let mut config = create_node_config_for_node(
            id,
            network_port,
            initial_peers,
            blend_port,
            base_consensus,
            base_time,
        )
        .map_err(|source| -> DynError { source.into() })?;

        let keys = &mut config.kms_config.backend.keys;
        for account in &descriptors.config().wallet_config.accounts {
            let key = account.secret_key.clone().into();
            let key_id = key_id_for_preload_backend(&key);
            keys.entry(key_id).or_insert(key);
        }

        config
    };

    Ok(PlannedLocalNodeConfig {
        config,
        descriptor_override: descriptors.config().node_config_override(index).cloned(),
        genesis_tx: descriptors
            .config()
            .genesis_tx
            .clone()
            .ok_or_else(|| io::Error::other("missing topology genesis tx"))?,
        port_strategy: PortStrategy::AllocateEphemeralPorts,
    })
}

pub fn build_node_run_config(
    topology: &DeploymentPlan,
    node: &NodePlan,
    descriptor_override: Option<&RunConfig>,
) -> Result<RunConfig, DynError> {
    if let Some(override_config) = descriptor_override {
        return Ok(override_config.clone());
    }

    let genesis_tx = topology
        .config()
        .genesis_tx
        .clone()
        .ok_or_else(|| io::Error::other("missing topology genesis tx"))?;
    Ok(build_run_config(node.general.clone(), genesis_tx))
}

fn finalize_dynamic_run_config(
    plan: &PlannedLocalNodeConfig,
    runtime_override: Option<&RunConfig>,
    template_config: Option<&RunConfig>,
) -> RunConfig {
    if let Some(override_config) = runtime_override {
        return override_config.clone();
    }

    if let Some(template_config) = template_config {
        return template_config.clone();
    }

    if let Some(override_config) = &plan.descriptor_override {
        return override_config.clone();
    }

    build_run_config(plan.config.clone(), plan.genesis_tx.clone())
}

fn build_run_config(config: Config, genesis_tx: GenesisTx) -> RunConfig {
    let deployment_config = default_e2e_deployment_settings(genesis_tx);

    let user_config = UserConfig {
        network: config.network_config,
        blend: config.blend_config.0,
        time: config.time_config,
        cryptarchia: build_cryptarchia_user_config(&config.consensus_config),
        tracing: config.tracing_config.tracing_settings,
        api: api::serde::Config {
            backend: api::serde::AxumBackendSettings {
                listen_address: config.api_config.address,
                max_concurrent_requests: 1000,
                ..Default::default()
            },
            testing: api::serde::AxumBackendSettings {
                listen_address: config.api_config.testing_http_address,
                max_concurrent_requests: 1000,
                ..Default::default()
            },
        },
        storage: storage::serde::Config::default(),
        sdp: sdp::serde::Config {
            declaration_id: None,
            wallet: sdp::serde::WalletConfig {
                max_tx_fee: mantle::Value::MAX,
                funding_pk: config.consensus_config.funding_sk.as_public_key(),
            },
        },
        wallet: wallet::serde::Config {
            known_keys: HashMap::from_iter([
                (
                    key_id_for_preload_backend(&Key::Zk(config.consensus_config.known_key.clone())),
                    config.consensus_config.known_key.as_public_key(),
                ),
                (
                    key_id_for_preload_backend(&Key::Zk(
                        config.consensus_config.funding_sk.clone(),
                    )),
                    config.consensus_config.funding_sk.as_public_key(),
                ),
            ]),
            voucher_master_key_id: key_id_for_preload_backend(&Key::Zk(
                config.consensus_config.known_key.clone(),
            )),
        },
        kms: config::kms::serde::Config {
            backend: config::kms::serde::PreloadKmsBackendSettings {
                keys: config.kms_config.backend.keys,
            },
        },
        state: state::Config::default(),
    };

    RunConfig {
        deployment: deployment_config,
        user: user_config,
    }
}

fn build_cryptarchia_user_config(
    consensus: &crate::node::configs::node_configs::consensus::GeneralConsensusConfig,
) -> config::cryptarchia::serde::Config {
    use std::{collections::HashSet, num::NonZeroUsize, time::Duration};

    use config::cryptarchia::serde::{
        Config as CryptarchiaConfig, leader, leader::Config as LeaderConfig, network,
        network::Config as NetworkConfig, service, service::Config as ServiceConfig,
    };

    CryptarchiaConfig {
        network: NetworkConfig {
            bootstrap: network::BootstrapConfig {
                ibd: network::IbdConfig {
                    delay_before_new_download: Duration::from_secs(10),
                    peers: HashSet::new(),
                },
            },
            network: network::NetworkConfig {
                max_connected_peers_to_try_download: 16,
                max_discovered_peers_to_try_download: 16,
            },
            sync: network::SyncConfig {
                orphan: network::OrphanConfig {
                    max_orphan_cache_size: NonZeroUsize::new(1000)
                        .expect("max orphan cache size must be non-zero"),
                },
            },
        },
        service: ServiceConfig {
            bootstrap: service::BootstrapConfig {
                force_bootstrap: false,
                offline_grace_period: service::OfflineGracePeriodConfig {
                    grace_period: Duration::from_secs(20 * 60),
                    state_recording_interval: Duration::from_secs(60),
                },
                prolonged_bootstrap_period: consensus.prolonged_bootstrap_period,
            },
        },
        leader: LeaderConfig {
            wallet: leader::WalletConfig {
                max_tx_fee: mantle::Value::MAX,
                funding_pk: consensus.funding_pk,
            },
        },
    }
}

fn resolve_initial_peers(
    peer_ports_by_name: &HashMap<String, u16>,
    peer_selection: &PeerSelection,
    default_peers: &[Multiaddr],
    descriptors: &DeploymentPlan,
    peer_ports: &[u16],
) -> Result<Vec<Multiaddr>, DynError> {
    match peer_selection {
        PeerSelection::Named(names) => {
            let mut peers = Vec::with_capacity(names.len());
            for name in names {
                let port = peer_ports_by_name
                    .get(name)
                    .ok_or_else(|| io::Error::other(format!("unknown peer name '{name}'")))?;
                peers.push(lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, *port));
            }

            Ok(peers)
        }
        PeerSelection::DefaultLayout => {
            if default_peers.is_empty() {
                let topology: &TopologyConfig = descriptors.config();
                Ok(initial_peers_for_dynamic_node(
                    topology.network_params.as_ref(),
                    peer_ports,
                ))
            } else {
                Ok(default_peers.to_vec())
            }
        }
        PeerSelection::None => Ok(Vec::new()),
    }
}

fn initial_peers_for_dynamic_node(
    network_params: &NetworkParams,
    peer_ports: &[u16],
) -> Vec<Multiaddr> {
    match network_params.libp2p_network_layout {
        Libp2pNetworkLayout::Star => peer_ports
            .first()
            .map(|port| vec![lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, *port)])
            .unwrap_or_default(),
        Libp2pNetworkLayout::Chain => peer_ports
            .last()
            .map(|port| vec![lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, *port)])
            .unwrap_or_default(),
        Libp2pNetworkLayout::Full => peer_ports
            .iter()
            .map(|port| lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, *port))
            .collect(),
    }
}
