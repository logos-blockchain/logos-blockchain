use std::{
    collections::HashMap,
    env, fs, io,
    net::{Ipv4Addr, UdpSocket},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use config::{api, sdp, state, storage, wallet};
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
    BinaryProviderRef, BuildBinaryProvider, BuildCommand, BuiltNodeConfig, DownloadBinaryProvider,
    DownloadChecksum, DownloadUrl, EnvBinaryProvider, FallbackBinaryProvider, LaunchEnvVar,
    LaunchFile, LocalDeployerEnv, NodeConfigEntry, NodeEndpointPort, NodeEndpoints,
    PathBinaryProvider, ProcessSpawnError, env::Node, process::LaunchSpec,
};
use tracing::debug;

use crate::{
    LOG_LEVEL,
    configs::deployment::NodeBinaryProfile,
    diagnostics::{
        record_system_monitor_event, register_system_monitor_output_file,
        unregister_system_monitor_output_file,
    },
    env as tf_env,
    env::{remove_default_env, replace_default_env},
    framework::LbcEnv,
    node::{
        DeploymentPlan, NodeHttpClient, NodePlan,
        configs::{
            Config, Libp2pNetworkLayout, NetworkParams, create_node_config_for_node,
            deployment::TopologyConfig, deployment_settings_for_topology,
        },
    },
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

    async fn build_launch_spec(
        config: &<Self as Application>::NodeConfig,
        dir: &Path,
        label: &str,
    ) -> Result<LaunchSpec, DynError> {
        let mut config = config.clone();

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

        build_node_launch_spec(dir, user_yaml, deployment_yaml).await
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

    fn node_peer_port(node: &Node<Self>) -> u16 {
        node.endpoints()
            .port(&NodeEndpointPort::Network)
            .unwrap_or_else(|| node.config().user.network.backend.swarm.port)
    }

    fn node_client(endpoints: &NodeEndpoints) -> Result<Self::NodeClient, DynError> {
        Ok(NodeHttpClient::new(endpoints.api))
    }

    fn readiness_endpoint_path() -> &'static str {
        "/cryptarchia/info"
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
) -> Result<LaunchSpec, DynError> {
    let config_path = dir.join(USER_CONFIG_FILE);
    let deployment_path = dir.join(DEPLOYMENT_CONFIG_FILE);
    let time_backend =
        env::var("LOGOS_BLOCKCHAIN_TIME_BACKEND").unwrap_or_else(|_| "monotonic".to_owned());
    let node_binary_profile =
        NodeBinaryProfile::from_string(&env::var("NODE_BINARY_PROFILE").unwrap_or_default());

    Ok(LaunchSpec {
        binary: {
            let provider = node_binary_provider(&node_binary_profile)?;
            let current = if node_binary_profile == NodeBinaryProfile::TokioConsole {
                replace_default_env("RUSTFLAGS", &rustflags_with_tokio_unstable())
            } else {
                None
            };
            let resolve_result = provider.resolve().await;
            if node_binary_profile == NodeBinaryProfile::TokioConsole {
                if let Some(val) = current {
                    let _unused = replace_default_env("RUSTFLAGS", &val);
                } else {
                    remove_default_env("RUSTFLAGS");
                }
            }
            resolve_result?
        },
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

fn rustflags_with_tokio_unstable() -> String {
    const TOKIO_UNSTABLE_CFG: &str = "--cfg tokio_unstable";

    match env::var("RUSTFLAGS") {
        Ok(flags) if flags.contains(TOKIO_UNSTABLE_CFG) => flags,
        Ok(flags) if flags.trim().is_empty() => TOKIO_UNSTABLE_CFG.to_owned(),
        Ok(flags) => format!("{flags} {TOKIO_UNSTABLE_CFG}"),
        Err(_) => TOKIO_UNSTABLE_CFG.to_owned(),
    }
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
    node_binary_provider(node_binary_profile)?
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
    if running_in_ci() {
        Arc::new(PathBinaryProvider::new(release_node_binary_path()))
    } else {
        Arc::new(BuildBinaryProvider {
            command: BuildCommand::new("cargo").with_args([
                "build",
                "--locked",
                "--profile",
                "release-profiling",
                "-p",
                "logos-blockchain-node",
                "--features",
                "testing,tokio-console",
            ]),
            output_path: release_profiling_node_binary_path(),
            working_dir: Some(workspace_root()),
            lock_dir: Some(workspace_root().join("target").join(".tf-binaries")),
        })
    }
}

fn release_profiling_node_binary_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("release-profiling")
        .join("logos-blockchain-node")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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
    template_config: Option<&RunConfig>,
) -> Result<BuiltNodeConfig<RunConfig>, DynError> {
    let plan = plan_local_node_config(
        topology,
        index,
        peer_ports_by_name,
        options.peers.as_ref(),
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
            declaration_id: config.sdp_config.declaration_id,
            wallet: sdp::serde::WalletConfig {
                max_tx_fee: mantle::Value::MAX.into(),
                funding_pk: config.consensus_config.funding_sk.as_public_key(),
            },
            active_message_tracker: ActiveMessageTrackerConfig {
                status_check_interval_in_tip_changes: NonZeroU64::new(3).unwrap(),
                max_status_checks: NonZeroU64::new(5).unwrap(),
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
        kms: config::kms::serde::Config {
            backend: config::kms::serde::PreloadKmsBackendSettings {
                keys: config.kms_config.backend.keys,
            },
        },
        // Pay PoW claim change to the node's own wallet key (node-controlled and
        // in the KMS/wallet, so spendable), not the SDP funding key.
        pow: config::pow::serde::Config::with_required_values(config::pow::serde::RequiredValues {
            claim_address: config.consensus_config.known_key.as_public_key(),
        }),
        state: state::Config::default(),
    };

    RunConfig {
        deployment: deployment_settings.clone(),
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
    use crate::node::configs::deployment::DeploymentBuilder;

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
