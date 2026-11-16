use std::{
    collections::HashMap,
    env, fs, io,
    net::{Ipv4Addr, UdpSocket},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use config::{api, mempool, sdp, state, storage, wallet};
use flate2::read::GzDecoder;
use lb_config::kms::key_id_for_preload_backend;
use lb_core::mantle;
use lb_key_management_system_service::keys::{Key, secured_key::SecuredKey as _};
use lb_libp2p::Multiaddr;
use lb_node::{
    UserConfig,
    config::{
        self, RunConfig,
        deployment::DeploymentSettings,
        sdp::serde::ActiveMessageTrackerConfig,
        tracing::serde::{
            Level,
            logger::{self, AppenderType},
        },
    },
};
use rand::Rng as _;
use testing_framework_core::scenario::{Application, DynError, PeerSelection, StartNodeOptions};
use testing_framework_runner_local::{
    BinaryProviderRef, BuildBinaryProvider, BuildCommand, DownloadBinaryProvider, DownloadChecksum,
    DownloadUrl, EnvBinaryProvider, FallbackBinaryProvider, LaunchEnvVar, LaunchFile,
    LocalBuildContext, LocalDeployerEnv, LocalPeerNode, NodeEndpointPort, NodeEndpoints,
    PathBinaryProvider, PreparedNode, ProcessSpawnError, env::Node, process::LaunchSpec,
};
use tracing::debug;

use crate::{
    DeploymentPlan, LOG_LEVEL, LbcEnv, LbcNodeConfig, NodeHttpClient, NodePlan,
    config::{
        Config, Libp2pNetworkLayout, NetworkParams, create_node_config_for_node,
        deployment::TopologyConfig, deployment_settings_for_topology,
    },
    configs::deployment::NodeBinaryProfile,
    diagnostics::{
        record_system_monitor_event, register_system_monitor_output_file,
        unregister_system_monitor_output_file,
    },
    env as tf_env,
};

const LOGS_PREFIX: &str = "__logs";
const DEFAULT_BLEND_NETWORK_PORT: u16 = 3400;
/// URL of a Logos node release binary downloaded by the local TF deployer.
///
/// Use an immutable URL, or set [`LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_SHA256`]. An
/// archive republished at the same URL is otherwise served from the download
/// cache without being fetched again.
pub const LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL: &str = "LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL";
/// Optional SHA-256 checksum for [`LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL`].
pub const LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_SHA256: &str = "LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_SHA256";
/// The default filename for the user config.
pub const USER_CONFIG_FILE: &str = "node.yaml";
/// The default filename for the deployment config.
pub const DEPLOYMENT_CONFIG_FILE: &str = "deployment.yaml";

struct PlannedLocalNodeConfig {
    config: Config,
    descriptor_override: Option<RunConfig>,
    deployment_settings: DeploymentSettings,
    port_strategy: PortStrategy,
}

#[derive(Clone, Copy)]
enum PortStrategy {
    PreservePlannedPorts,
    AllocateEphemeralPorts,
}

#[async_trait]
impl LocalDeployerEnv for LbcEnv {
    fn prepare_local_cluster(topology: &Self::Deployment) {
        register_system_monitor_output_file(
            &topology
                .config()
                .scenario_base_dir
                .join("system_stats.ndjson"),
        );
    }

    fn cleanup_local_cluster(topology: &Self::Deployment) {
        unregister_system_monitor_output_file(
            &topology
                .config()
                .scenario_base_dir
                .join("system_stats.ndjson"),
        );
    }

    fn build_node_config(
        context: LocalBuildContext<'_, Self>,
    ) -> Result<PreparedNode<Self::NodeConfig>, DynError> {
        let peer_ports = context
            .peers
            .iter()
            .map(LocalPeerNode::network_port)
            .collect::<Vec<_>>();
        let peer_ports_by_name = context
            .peers
            .iter()
            .filter_map(|peer| {
                peer.name()
                    .map(|name| (name.to_owned(), peer.network_port()))
            })
            .collect();
        build_dynamic_node_config(
            context.topology,
            context.index,
            &peer_ports_by_name,
            context.options,
            &peer_ports,
            context.template_config,
        )
    }

    fn build_initial_node_configs(
        topology: &Self::Deployment,
    ) -> Result<Vec<PreparedNode<<Self as Application>::NodeConfig>>, ProcessSpawnError> {
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
                Ok::<_, ProcessSpawnError>(PreparedNode {
                    name: label,
                    network_port: config.user.network.backend.swarm.port,
                    config: LbcNodeConfig {
                        run_config: config,
                        binary_profile: topology.config().node_binary_profile().clone(),
                    },
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

    async fn build_launch_spec(
        config: &<Self as Application>::NodeConfig,
        dir: &Path,
        label: &str,
    ) -> Result<LaunchSpec, DynError> {
        let binary_profile = config.binary_profile.clone();
        let mut config = config.run_config.clone();

        record_system_monitor_event(
            "node_runtime_prepared",
            format!("{label}:{}", dir.display()),
        );

        if let Some(level) = configured_node_log_level() {
            config.user.tracing.level = level;
        }

        if !tf_env::debug_tracing() {
            let log_prefix = format!("{LOGS_PREFIX}-{label}");
            config.user.tracing.logger = configure_logging(dir, &log_prefix);
        }

        config.user.state.base_folder = dir.to_path_buf();
        "db".clone_into(&mut config.user.storage.backend.folder_name);

        if let config::tracing::serde::console::Layer::Console(console) =
            &mut config.user.tracing.console
            && let Some(recording_path) = &mut console.recording_path
        {
            if recording_path.is_relative() {
                let relative_path = recording_path.clone();
                *recording_path = dir.join(relative_path);
            }
            if let Some(parent) = recording_path.parent() {
                fs::create_dir_all(parent).map_err(|source| {
                    io::Error::other(format!(
                        "failed to prepare Tokio console recording path `{}`: {source}",
                        recording_path.display()
                    ))
                })?;
            }
        }

        let user_yaml = serde_yaml::to_string(&config.user).map_err(io::Error::other)?;
        let deployment_yaml =
            serde_yaml::to_string(&config.deployment).map_err(io::Error::other)?;

        build_node_launch_spec(dir, user_yaml, deployment_yaml, &binary_profile).await
    }

    fn node_endpoints(
        config: &<Self as Application>::NodeConfig,
    ) -> Result<NodeEndpoints, DynError> {
        let mut endpoints = NodeEndpoints {
            api: config.user.api.backend.listen_address,
            ..Default::default()
        };

        add_endpoint_ports(&mut endpoints, config);

        Ok(endpoints)
    }

    fn node_client(endpoints: &NodeEndpoints) -> Result<Self::NodeClient, DynError> {
        Ok(NodeHttpClient::new(endpoints.api))
    }

    async fn wait_readiness_stable(nodes: &[Node<Self>]) -> Result<(), DynError> {
        super::readiness::wait_readiness_stable(nodes).await
    }
}

fn add_endpoint_ports(endpoints: &mut NodeEndpoints, config: &RunConfig) {
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

async fn build_node_launch_spec(
    dir: &Path,
    user_yaml: String,
    deployment_yaml: String,
    node_binary_profile: &NodeBinaryProfile,
) -> Result<LaunchSpec, DynError> {
    build_node_launch_spec_with(
        dir,
        user_yaml,
        deployment_yaml,
        node_binary_profile,
        node_binary_provider,
    )
    .await
}

async fn build_node_launch_spec_with(
    dir: &Path,
    user_yaml: String,
    deployment_yaml: String,
    node_binary_profile: &NodeBinaryProfile,
    provider_for_profile: impl FnOnce(&NodeBinaryProfile) -> Result<BinaryProviderRef, DynError>,
) -> Result<LaunchSpec, DynError> {
    let config_path = dir.join(USER_CONFIG_FILE);
    let deployment_path = dir.join(DEPLOYMENT_CONFIG_FILE);
    let time_backend =
        env::var("LOGOS_BLOCKCHAIN_TIME_BACKEND").unwrap_or_else(|_| "monotonic".to_owned());
    Ok(LaunchSpec {
        binary: resolve_node_binary_with(node_binary_profile, provider_for_profile).await?,
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

/// Resolves (building if necessary) the node binary for `node_binary_profile`
/// and populates the process-local binary cache.
pub async fn ensure_node_binary_built(
    node_binary_profile: &NodeBinaryProfile,
) -> Result<PathBuf, DynError> {
    resolve_node_binary_with(node_binary_profile, node_binary_provider).await
}

async fn resolve_node_binary_with(
    node_binary_profile: &NodeBinaryProfile,
    provider_for_profile: impl FnOnce(&NodeBinaryProfile) -> Result<BinaryProviderRef, DynError>,
) -> Result<PathBuf, DynError> {
    provider_for_profile(node_binary_profile)?
        .resolve()
        .await
        .map_err(Into::into)
}

fn node_binary_provider(
    node_binary_profile: &NodeBinaryProfile,
) -> Result<BinaryProviderRef, DynError> {
    let release_download_requested =
        env::var_os(LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL).is_some_and(|url| !url.is_empty());
    validate_node_binary_selection(node_binary_profile, release_download_requested)?;

    let mut providers: Vec<BinaryProviderRef> = vec![Arc::new(EnvBinaryProvider::new(
        "LOGOS_BLOCKCHAIN_NODE_BIN",
    ))];

    if release_download_requested {
        providers.push(Arc::new(release_binary_provider()));
    }

    providers.push(match node_binary_profile {
        NodeBinaryProfile::Normal => default_node_binary_provider(),
        NodeBinaryProfile::TokioConsole => tokio_console_node_binary_provider(),
    });

    Ok(Arc::new(FallbackBinaryProvider::new(providers)))
}

fn validate_node_binary_selection(
    node_binary_profile: &NodeBinaryProfile,
    release_download_requested: bool,
) -> Result<(), DynError> {
    if release_download_requested && *node_binary_profile == NodeBinaryProfile::TokioConsole {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL} cannot be used with the tokio-console node binary profile"
            ),
        )
        .into());
    }

    Ok(())
}

fn release_binary_provider() -> DownloadBinaryProvider {
    DownloadBinaryProvider {
        url: DownloadUrl::Env(LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL.to_owned()),
        sha256: Some(DownloadChecksum::Env(
            LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_SHA256.to_owned(),
        )),
        cache_dir: None,
        processor: None,
    }
    .with_processor_fn("logos-node-tar-gz-v1", extract_release_binary)
}

fn extract_release_binary(artifact: &Path, output: &Path) -> Result<(), DynError> {
    let decoder = GzDecoder::new(fs::File::open(artifact)?);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry
            .path()?
            .file_name()
            .is_some_and(|name| name == "logos-blockchain-node")
        {
            entry.unpack(output)?;
            return Ok(());
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "release archive does not contain logos-blockchain-node",
    )
    .into())
}

fn default_node_binary_provider() -> BinaryProviderRef {
    if running_in_ci() {
        Arc::new(PathBinaryProvider::new(release_node_binary_path()))
    } else {
        Arc::new(BuildBinaryProvider {
            command: BuildCommand::new("cargo").with_args([
                "build",
                "--locked",
                "--release",
                "-p",
                "logos-blockchain-node",
                "--features",
                "testing",
            ]),
            output_path: release_node_binary_path(),
            working_dir: Some(workspace_root()),
            lock_dir: Some(workspace_root().join("target").join(".tf-binaries")),
        })
    }
}

fn release_node_binary_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("release")
        .join("logos-blockchain-node")
}

fn tokio_console_node_binary_provider() -> BinaryProviderRef {
    Arc::new(BuildBinaryProvider {
        command: tokio_console_build_command([
            "build".to_owned(),
            "--locked".to_owned(),
            "--profile".to_owned(),
            "release-profiling".to_owned(),
            "-p".to_owned(),
            "logos-blockchain-node".to_owned(),
            "--features".to_owned(),
            "testing,tokio-console".to_owned(),
        ]),
        output_path: release_profiling_node_binary_path(),
        working_dir: Some(workspace_root()),
        lock_dir: Some(workspace_root().join("target").join(".tf-binaries")),
    })
}

fn tokio_console_build_command(args: impl IntoIterator<Item = String>) -> BuildCommand {
    BuildCommand::new("env").with_args(
        [
            format!(
                "LOGOS_ORIGINAL_RUSTC_WRAPPER={}",
                env::var("RUSTC_WRAPPER")
                    .or_else(|_| env::var("CARGO_BUILD_RUSTC_WRAPPER"))
                    .unwrap_or_default()
            ),
            format!(
                "RUSTC_WRAPPER={}",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/tokio_console_rustc_wrapper.sh")
                    .display()
            ),
            "cargo".to_owned(),
        ]
        .into_iter()
        .chain(args),
    )
}

fn release_profiling_node_binary_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("release-profiling")
        .join("logos-blockchain-node")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn running_in_ci() -> bool {
    env::var_os("CI").is_some() || env::var_os("GITHUB_ACTIONS").is_some()
}

fn configure_logging(base_dir: &Path, prefix: &str) -> logger::Layers {
    debug!(prefix, base_dir = %base_dir.display(), "configuring node logging");

    if let Some(log_dir) = tf_env::logos_blockchain_log_dir() {
        match fs::create_dir_all(&log_dir) {
            Ok(()) => {
                return logger::Layers {
                    file: Some(logger::FileConfig {
                        directory: log_dir,
                        prefix: Some(prefix.into()),
                        appender_type: AppenderType::Simple,
                    }),
                    loki: None,
                    gelf: None,
                    otlp: None,
                    format: logger::LogFormat::default(),
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
            appender_type: AppenderType::Simple,
        }),
        loki: None,
        gelf: None,
        otlp: None,
        format: logger::LogFormat::default(),
        stdout: false,
        stderr: false,
    }
}

fn configured_node_log_level() -> Option<Level> {
    env::var(LOG_LEVEL)
        .ok()
        .map(|level| level.parse::<Level>().unwrap_or(Level::INFO))
}

fn build_dynamic_node_config(
    topology: &DeploymentPlan,
    index: usize,
    peer_ports_by_name: &HashMap<String, u16>,
    options: &StartNodeOptions<LbcEnv>,
    peer_ports: &[u16],
    template_config: Option<&LbcNodeConfig>,
) -> Result<PreparedNode<LbcNodeConfig>, DynError> {
    let plan = plan_local_node_config(
        topology,
        index,
        peer_ports_by_name,
        options.common.peers.as_ref(),
        peer_ports,
    )?;
    let mut config = finalize_dynamic_run_config(
        &plan,
        options.config_override.as_ref().map(|c| &c.run_config),
        template_config.map(|c| &c.run_config),
    );
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

    Ok(PreparedNode {
        name: format!("node-{index}"),
        config: LbcNodeConfig {
            run_config: config,
            binary_profile: topology.config().node_binary_profile().clone(),
        },
        network_port,
    })
}

fn plan_local_node_config(
    descriptors: &DeploymentPlan,
    index: usize,
    peer_ports_by_name: &HashMap<String, u16>,
    peer_selection: Option<&PeerSelection>,
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

        let genesis_block = descriptors
            .config()
            .genesis_block
            .as_ref()
            .ok_or_else(|| io::Error::other("missing topology genesis tx"))?;

        return Ok(PlannedLocalNodeConfig {
            config,
            descriptor_override: descriptors.config().node_config_override(index).cloned(),
            deployment_settings: deployment_settings_for_topology(
                genesis_block,
                descriptors.config(),
            ),
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
            descriptors.config.test_context.as_deref(),
            descriptors.config.genesis_time(),
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

    let genesis_block = descriptors
        .config()
        .genesis_block
        .as_ref()
        .ok_or_else(|| io::Error::other("missing topology genesis tx"))?;

    Ok(PlannedLocalNodeConfig {
        config,
        descriptor_override: descriptors.config().node_config_override(index).cloned(),
        deployment_settings: deployment_settings_for_topology(genesis_block, descriptors.config()),
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

    let genesis_block = topology
        .config()
        .genesis_block
        .clone()
        .ok_or_else(|| io::Error::other("missing topology genesis tx"))?;
    let deployment_settings = deployment_settings_for_topology(&genesis_block, topology.config());
    Ok(build_run_config(node.general.clone(), &deployment_settings))
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

    build_run_config(plan.config.clone(), &plan.deployment_settings)
}

fn build_run_config(config: Config, deployment_settings: &DeploymentSettings) -> RunConfig {
    let mut tracing = config.tracing_config.tracing_settings;
    tracing.level = Level::INFO;

    let user_config = UserConfig {
        network: config.network_config,
        blend: config.blend_config.0,
        time: config.time_config,
        cryptarchia: build_cryptarchia_user_config(&config.consensus_config),
        tracing,
        api: api::serde::Config {
            backend: api::serde::AxumBackendSettings {
                listen_address: config.api_config.address,
                max_concurrent_requests: 1000,
                ..Default::default()
            },
        },
        storage: storage::serde::Config::default(),
        sdp: sdp::serde::Config {
            declaration: config.sdp_config.declaration.map(Into::into),
            wallet: sdp::serde::WalletConfig {
                max_tx_fee: mantle::Value::MAX.into(),
                funding_pk: config.consensus_config.funding_sk.as_public_key(),
            },
            active_message_tracker: ActiveMessageTrackerConfig {
                status_check_interval_in_tip_changes: NonZeroU64::new(3).unwrap(),
            },
        },
        wallet: {
            let known_keys: HashMap<_, _> = [
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
            ]
            .into_iter()
            .chain(config.consensus_config.other_keys.iter().map(|sk| {
                (
                    key_id_for_preload_backend(&sk.clone().into()),
                    sk.as_public_key(),
                )
            }))
            .chain(
                config
                    .kms_config
                    .backend
                    .keys
                    .values()
                    .filter_map(|key| match key {
                        Key::Zk(sk) => Some((
                            key_id_for_preload_backend(&Key::Zk(sk.clone())),
                            sk.as_public_key(),
                        )),
                        Key::Ed25519(_) => None,
                    }),
            )
            .collect();

            wallet::serde::Config {
                known_keys,
                ..wallet::serde::Config::with_required_values(wallet::serde::RequiredValues {
                    voucher_master_key_id: key_id_for_preload_backend(&Key::Zk(
                        config.consensus_config.known_key.clone(),
                    )),
                })
            }
        },
        mempool: mempool::serde::Config::default(),
        kms: config::kms::serde::Config {
            backend: config::kms::serde::PreloadKmsBackendSettings {
                keys: config.kms_config.backend.keys,
            },
            ..config::kms::serde::Config::default()
        },
        // Mining defaults, auto-claim off: provisioned nodes claim on demand,
        // naming the destination key on each claim request.
        pow: config::pow::serde::Config::default(),
        state: state::Config::default(),
    };

    RunConfig {
        deployment: deployment_settings.clone(),
        user: user_config,
    }
}

fn build_cryptarchia_user_config(
    consensus: &crate::config::node_configs::consensus::GeneralConsensusConfig,
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
                    peers: HashSet::new(),
                    delay_before_new_download: Duration::from_secs(10),
                    tips_fetch_max_attempts: 3,
                    tips_fetch_min_delay: Duration::from_millis(250),
                    tips_fetch_max_delay: Duration::from_secs(1),
                    round_delay: Duration::from_secs(1),
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
                    max_rejected_cache_size: 1000,
                },
                tip_poll: network::TipPollConfig::default(),
            },
            seen_items_subscription_channel_size: NonZeroU64::new(64)
                .expect("received proposals buffer must be non-zero"),
        },
        service: ServiceConfig {
            bootstrap: service::BootstrapConfig {
                force_bootstrap: false,
                offline_grace_period: service::OfflineGracePeriodConfig {
                    grace_period: Duration::from_mins(20),
                    state_recording_interval: Duration::from_mins(1),
                },
                prolonged_bootstrap_period: consensus.prolonged_bootstrap_period,
            },
            sync: service::SyncConfig {
                block_provider: service::BlockProviderConfig {
                    batch_size: NonZeroUsize::new(1000)
                        .expect("block_provider batch_size must be non-zero"),
                },
            },
        },
        leader: LeaderConfig {
            wallet: leader::WalletConfig {
                max_tx_fee: mantle::Value::MAX.into(),
                funding_pk: consensus.funding_pk,
            },
        },
    }
}

fn resolve_initial_peers(
    peer_ports_by_name: &HashMap<String, u16>,
    peer_selection: Option<&PeerSelection>,
    default_peers: &[Multiaddr],
    descriptors: &DeploymentPlan,
    peer_ports: &[u16],
) -> Result<Vec<Multiaddr>, DynError> {
    match peer_selection {
        Some(PeerSelection::Named(names)) => {
            let mut peers = Vec::with_capacity(names.len());
            for name in names {
                let port = peer_ports_by_name
                    .get(name)
                    .ok_or_else(|| io::Error::other(format!("unknown peer name '{name}'")))?;
                peers.push(lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, *port));
            }

            Ok(peers)
        }
        None | Some(PeerSelection::DefaultLayout) => {
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
        Some(PeerSelection::None) => Ok(Vec::new()),
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

#[cfg(test)]
mod tests {
    use flate2::{Compression, write::GzEncoder};

    use super::*;
    use crate::config::deployment::DeploymentBuilder;

    #[test]
    fn external_profile_override_takes_precedence() {
        use NodeBinaryProfile::{Normal, TokioConsole};
        assert_eq!(NodeBinaryProfile::resolve(None, Normal), Normal);
        assert_eq!(NodeBinaryProfile::resolve(None, TokioConsole), TokioConsole);
        assert_eq!(
            NodeBinaryProfile::resolve(Some("normal"), TokioConsole),
            Normal
        );
        assert_eq!(
            NodeBinaryProfile::resolve(Some("tokio-console"), Normal),
            TokioConsole
        );
    }

    #[test]
    fn deployment_profile_reaches_initial_local_node_configuration() {
        let original_profile = env::var_os("NODE_BINARY_PROFILE");
        for profile in [NodeBinaryProfile::Normal, NodeBinaryProfile::TokioConsole] {
            let topology = DeploymentBuilder::new(
                TopologyConfig::with_node_numbers(1)
                    .with_resolved_node_binary_profile(profile.clone()),
            )
            .build()
            .expect("deployment");
            let nodes = LbcEnv::build_initial_node_configs(&topology).expect("node configs");
            assert_eq!(nodes[0].config.binary_profile, profile);
        }
        assert_eq!(env::var_os("NODE_BINARY_PROFILE"), original_profile);
    }

    #[test]
    fn runtime_metadata_reflects_profile_and_applied_security_override() {
        use blockchain_test_support::runtime_info::NodeRuntimeInfoProvider as _;

        use crate::config::deployment::CucumberParameterSet;
        for (name, expected) in [
            ("cucumber_fixture", 8),
            ("fast_repro", 3),
            ("testnet_representative", 10),
        ] {
            let profile = CucumberParameterSet::from_name(name).unwrap();
            let topology = DeploymentBuilder::new(
                TopologyConfig::with_node_numbers(1).with_parameter_set(profile),
            )
            .build()
            .unwrap();
            let mut config = LbcEnv::build_initial_node_configs(&topology)
                .unwrap()
                .remove(0)
                .config;
            assert_eq!(
                LbcEnv::runtime_info(&config)
                    .unwrap()
                    .cryptarchia_security_param
                    .unwrap()
                    .get(),
                expected
            );
            config
                .deployment
                .update_genesis_era_parameters(|parameters| {
                    parameters.cryptarchia.security_param = std::num::NonZeroU32::new(30).unwrap();
                })
                .unwrap();
            assert_eq!(
                LbcEnv::runtime_info(&config)
                    .unwrap()
                    .cryptarchia_security_param
                    .unwrap()
                    .get(),
                30
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "The fixture exercises both complete profile orders and their launch paths"
    )]
    #[cfg(unix)]
    #[tokio::test]
    async fn alternating_profiles_build_and_reuse_distinct_executables() {
        use std::os::unix::fs::PermissionsExt as _;

        let parent_rustflags = env::var_os("RUSTFLAGS");
        let parent_encoded = env::var_os("CARGO_ENCODED_RUSTFLAGS");
        for tokio_first in [false, true] {
            let dir = tempfile::tempdir().expect("fixture directory");
            fs::create_dir_all(dir.path().join("src")).expect("source directory");
            fs::write(dir.path().join("Cargo.toml"), "[package]\nname='profile-fixture'\nversion='0.1.0'\nedition='2021'\n[profile.release-profiling]\ninherits='release'\n").expect("manifest");
            fs::write(
                dir.path().join("src/main.rs"),
                "#![allow(unexpected_cfgs)]\nfn main() { println!(\"{}:{}\", cfg!(tokio_unstable), cfg!(fixture_user_flag)); }\n",
            )
            .expect("source");
            let target = dir.path().join("target");
            let compile_log = dir.path().join("compiler-invocations");
            let wrapper = dir.path().join("count-compiler.sh");
            fs::write(&wrapper, "#!/bin/sh\ncase \" $* \" in *' --crate-name profile_fixture '*) printf 'compile\\n' >> \"$PROFILE_COMPILER_LOG\";; esac\nexec \"$@\"\n").expect("compiler counter");
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
                .expect("executable counter");
            let args = |profile: &str| {
                vec![
                    "build".to_owned(),
                    "--offline".to_owned(),
                    "--quiet".to_owned(),
                    "--profile".to_owned(),
                    profile.to_owned(),
                    "--target-dir".to_owned(),
                    target.display().to_string(),
                ]
            };
            let normal = BuildBinaryProvider {
                command: BuildCommand::new("env").with_args(
                    [
                        "-u".to_owned(),
                        "CARGO_ENCODED_RUSTFLAGS".to_owned(),
                        "RUSTFLAGS=--cfg=fixture_user_flag".to_owned(),
                        format!("RUSTC_WRAPPER={}", wrapper.display()),
                        format!("PROFILE_COMPILER_LOG={}", compile_log.display()),
                        "cargo".to_owned(),
                    ]
                    .into_iter()
                    .chain(args("release")),
                ),
                output_path: target.join("release/profile-fixture"),
                working_dir: Some(dir.path().to_owned()),
                lock_dir: Some(dir.path().join("locks")),
            };
            let mut tokio_command = tokio_console_build_command(args("release-profiling"));
            let original = tokio_command
                .args
                .iter_mut()
                .find(|arg| arg.starts_with("LOGOS_ORIGINAL_RUSTC_WRAPPER="))
                .expect("wrapper setting");
            *original = format!("LOGOS_ORIGINAL_RUSTC_WRAPPER={}", wrapper.display());
            tokio_command.args.splice(
                0..0,
                [
                    "-u".to_owned(),
                    "CARGO_ENCODED_RUSTFLAGS".to_owned(),
                    "RUSTFLAGS=--cfg=fixture_user_flag".to_owned(),
                    format!("PROFILE_COMPILER_LOG={}", compile_log.display()),
                ],
            );
            let tokio = BuildBinaryProvider {
                command: tokio_command,
                output_path: target.join("release-profiling/profile-fixture"),
                working_dir: Some(dir.path().to_owned()),
                lock_dir: Some(dir.path().join("locks")),
            };
            let normal: BinaryProviderRef = Arc::new(normal);
            let tokio: BinaryProviderRef = Arc::new(tokio);
            let provider = |profile: &NodeBinaryProfile| {
                Ok(match profile {
                    NodeBinaryProfile::Normal => Arc::clone(&normal),
                    NodeBinaryProfile::TokioConsole => Arc::clone(&tokio),
                })
            };
            let sequence = if tokio_first {
                [
                    NodeBinaryProfile::TokioConsole,
                    NodeBinaryProfile::Normal,
                    NodeBinaryProfile::TokioConsole,
                    NodeBinaryProfile::Normal,
                ]
            } else {
                [
                    NodeBinaryProfile::Normal,
                    NodeBinaryProfile::TokioConsole,
                    NodeBinaryProfile::Normal,
                    NodeBinaryProfile::TokioConsole,
                ]
            };
            let mut paths = Vec::new();
            for (index, profile) in sequence.into_iter().enumerate() {
                let path = resolve_node_binary_with(&profile, provider)
                    .await
                    .expect("preflight fixture binary");
                assert!(path.is_file());
                let expected_compiles = (index + 1).min(2);
                assert_eq!(
                    fs::read_to_string(&compile_log).unwrap().lines().count(),
                    expected_compiles,
                    "only each profile's first preflight should compile"
                );
                let launch = build_node_launch_spec_with(
                    dir.path(),
                    "user".into(),
                    "deployment".into(),
                    &profile,
                    provider,
                )
                .await
                .expect("launch specification");
                assert_eq!(launch.binary, path);
                assert_eq!(
                    fs::read_to_string(&compile_log).unwrap().lines().count(),
                    expected_compiles,
                    "launch resolution must not recompile"
                );
                paths.push(path);
            }
            assert_eq!(paths[0], paths[2]);
            assert_eq!(paths[1], paths[3]);
            assert_ne!(paths[0], paths[1]);
            let output =
                std::process::Command::new(tokio.resolve().await.expect("cached profiling binary"))
                    .output()
                    .expect("fixture runs");
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "true:true");
            let output =
                std::process::Command::new(normal.resolve().await.expect("cached normal binary"))
                    .output()
                    .expect("normal fixture runs");
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "false:true");
        }
        assert_eq!(env::var_os("RUSTFLAGS"), parent_rustflags);
        assert_eq!(env::var_os("CARGO_ENCODED_RUSTFLAGS"), parent_encoded);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tokio_build_retains_cargo_target_rustflags() {
        let dir = tempfile::tempdir().expect("fixture directory");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='target-flags-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(".cargo/config.toml"),
            "[target.'cfg(unix)']\nrustflags = ['--cfg=fixture_target_flag']\n",
        )
        .unwrap();
        fs::write(dir.path().join("src/main.rs"), "#![allow(unexpected_cfgs)]\nfn main() { println!(\"{}:{}\", cfg!(tokio_unstable), cfg!(fixture_target_flag)); }\n").unwrap();
        let target = dir.path().join("target");
        let mut command = tokio_console_build_command([
            "build".to_owned(),
            "--offline".to_owned(),
            "--quiet".to_owned(),
            "--target-dir".to_owned(),
            target.display().to_string(),
        ]);
        command.args.splice(
            0..0,
            [
                "-u".to_owned(),
                "RUSTFLAGS".to_owned(),
                "-u".to_owned(),
                "CARGO_ENCODED_RUSTFLAGS".to_owned(),
            ],
        );
        let provider: BinaryProviderRef = Arc::new(BuildBinaryProvider {
            command,
            output_path: target.join("debug/target-flags-fixture"),
            working_dir: Some(dir.path().to_owned()),
            lock_dir: Some(dir.path().join("locks")),
        });
        let output = std::process::Command::new(provider.resolve().await.unwrap())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "true:true");
    }

    #[test]
    fn release_download_rejects_tokio_console_profile() {
        let error = validate_node_binary_selection(&NodeBinaryProfile::TokioConsole, true)
            .expect_err("downloaded releases do not include tokio-console support");

        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::InvalidInput)
        );
        assert!(
            error
                .to_string()
                .contains(LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL)
        );
    }

    #[test]
    fn extracts_node_binary_from_release_archive() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be created");
        let archive = temp_dir.path().join("release.tar.gz");
        let output = temp_dir.path().join("logos-blockchain-node");
        write_tar_gz(
            &archive,
            &[(
                "logos-blockchain-node/target/release/logos-blockchain-node",
                b"node binary",
            )],
        );

        extract_release_binary(&archive, &output).expect("node binary should be extracted");

        assert_eq!(
            fs::read(output).expect("extracted binary should be readable"),
            b"node binary"
        );
    }

    #[test]
    fn release_archive_without_node_binary_is_rejected() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be created");
        let archive = temp_dir.path().join("release.tar.gz");
        let output = temp_dir.path().join("logos-blockchain-node");
        write_tar_gz(&archive, &[("release/README.md", b"missing binary")]);

        let error = extract_release_binary(&archive, &output)
            .expect_err("archive without the node binary should fail");

        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::NotFound)
        );
        assert!(!output.exists());
    }

    fn write_tar_gz(path: &Path, entries: &[(&str, &[u8])]) {
        let file = fs::File::create(path).expect("archive file should be created");
        let encoder = GzEncoder::new(file, Compression::default());
        let mut archive = tar::Builder::new(encoder);

        for (entry_path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, entry_path, *contents)
                .expect("archive entry should be written");
        }

        let encoder = archive.into_inner().expect("archive should be finalized");
        encoder.finish().expect("gzip stream should be finalized");
    }

    #[test]
    fn local_cluster_lifecycle_unregisters_system_monitor_output() {
        let state_dir = tempfile::tempdir().expect("state directory should be created");
        let output = state_dir.path().join("system_stats.ndjson");
        let deployment = DeploymentBuilder::new(TopologyConfig::with_node_numbers(1))
            .scenario_base_dir(state_dir.path().to_owned())
            .build()
            .expect("deployment should build");

        LbcEnv::prepare_local_cluster(&deployment);
        assert!(output.exists());

        LbcEnv::cleanup_local_cluster(&deployment);
        fs::remove_file(&output).expect("monitor output should be removable after cleanup");
        record_system_monitor_event("after_cluster_cleanup", "test");

        assert!(!output.exists(), "cleaned-up output must not be recreated");
    }
}
