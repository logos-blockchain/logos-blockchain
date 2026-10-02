use std::sync::{Arc, OnceLock};

use super::*;
use crate::cucumber::{
    deployment::{LocalDeployment, runtime_info::NodeRuntimeInfo},
    steps::nodes::diagnostics::log_blend_relay_event,
};

const NODE_RESTART_GRACE_PERIOD: Duration = Duration::from_secs(3);

// Sort nodes_to_start with empty peers first to ensure standalone nodes start
// before connected nodes, then by dependency order to ensure all peers of a
// node are started before the node itself is started. If there is a circular
// dependency, return an error.
pub fn start_nodes_order_respecting_dependencies(
    nodes_to_start: NodesToStartUnordered,
    already_started: HashSet<String>,
) -> Result<NodesToStartOrdered, StepError> {
    let mut remaining = nodes_to_start;
    // Peers that are already running (started by earlier steps) count as
    // satisfied dependencies, so a node in this batch may connect to them.
    let mut started = already_started;
    let mut ordered = Vec::new();

    // Step 1: Find all nodes whose peer dependencies are already satisfied
    // (no in-batch peers, or all peers already running).
    let nodes_without_peers: Vec<String> = remaining
        .iter()
        .filter(|&(_, (_, peers))| peers.iter().all(|peer| started.contains(peer)))
        .map(|(node_name, (_, _))| node_name.clone())
        .collect();

    if nodes_without_peers.is_empty() && !remaining.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "No nodes without peer dependencies found. Possible circular dependency."
                .to_owned(),
        });
    }

    // Update start list with all nodes without peers
    for node_name in nodes_without_peers {
        if let Some((wallet_infos, initial_peers)) = remaining.remove(&node_name) {
            ordered.push((node_name.clone(), wallet_infos, initial_peers));
            started.insert(node_name);
        }
    }

    // Step 2: Iteratively find nodes whose peer dependencies are already included
    // in the start list
    while !remaining.is_empty() {
        let mut made_progress = false;

        let ready_nodes: Vec<String> = remaining
            .iter()
            .filter_map(|(node_name, (_, peers))| {
                let all_peers_started = peers.iter().all(|peer| started.contains(peer));
                all_peers_started.then(|| node_name.clone())
            })
            .collect();

        for node_name in ready_nodes {
            if let Some((wallet_infos, mut peers)) = remaining.remove(&node_name) {
                peers.sort();
                peers.dedup();
                ordered.push((node_name.clone(), wallet_infos, peers));
                started.insert(node_name);
                made_progress = true;
            }
        }

        if !made_progress {
            let remaining_nodes: Vec<String> = remaining.keys().cloned().collect();
            return Err(StepError::InvalidArgument {
                message: format!("Circular dependency detected among nodes: {remaining_nodes:?}"),
            });
        }
    }

    Ok(ordered)
}

#[expect(
    clippy::too_many_lines,
    reason = "Covers startup, optional snapshot seeding, wallet wiring, and readiness in one path"
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub async fn start_node(
    world: &mut CucumberWorld,
    step: &str,
    node_name: &str,
    wallet_start_info: &[WalletStartInfo],
    initial_peers: &[String],
    immediate_start: bool,
    extra_user_overrides: &[ConfigOverride],
) -> StepResult {
    let mut startup_settings =
        get_startup_settings(world, initial_peers, node_name).inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    // Merge per-node user config overrides (e.g. a mining node's derived
    // `pow.auto_claim`) on top of the scenario-wide ones, upserting by path.
    for extra in extra_user_overrides {
        if let Some(existing) = startup_settings
            .user_config_overrides
            .iter_mut()
            .find(|item| item.path == extra.path)
        {
            existing.value = extra.value.clone();
        } else {
            startup_settings.user_config_overrides.push(extra.clone());
        }
    }

    let is_bootstrap_node = startup_settings.is_bootstrap_node;
    let join_external_network = startup_settings.join_external_network;

    let persist_dir = world.lifecycle.scenario_base_dir.join(node_name);
    let runtime_dir_prefix = format!("{node_name}_");
    let final_dir_ignore_list = matching_child_dirs(&persist_dir, &runtime_dir_prefix);
    let tokio_console_node = startup_settings.tokio_console_node.clone();

    let blend_relays = world.blend_relays.clone();
    let relay_node_name = node_name.to_owned();
    let relay_preexisting = world.blend_relays.metadata(node_name)?.is_some();

    let scenario_wallet_key_ids = world
        .wallet_registry
        .wallet_accounts
        .values()
        .map(wallet_account_key_id)
        .chain(
            world
                .wallet_registry
                .fee_state
                .wallet_account
                .iter()
                .map(wallet_account_key_id),
        )
        .collect();

    let common_options = NodeLaunchOptions::default()
        .with_peers(startup_settings.peer_selection.clone())
        .with_persist_dir(persist_dir);

    let effective_runtime_info = Arc::new(OnceLock::new());
    let logos_start_options = startup_settings.logos_start_options(
        scenario_wallet_key_ids,
        blend_relays,
        relay_node_name,
        Arc::clone(&effective_runtime_info),
    )?;

    let start_result = launch_local_node(
        world,
        node_name,
        common_options,
        logos_start_options,
        &effective_runtime_info,
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    });

    let (started_node_name, client, runtime_info) = match start_result {
        Ok(started) => started,
        Err(error) => {
            if !relay_preexisting
                && let Err(cleanup_error) = world.blend_relays.remove_provider(node_name)
            {
                warn!(
                    target: TARGET,
                    node = node_name,
                    error = %cleanup_error,
                    "Failed to remove provisional Blend relay after node-start failure"
                );
            }
            return Err(error);
        }
    };
    world.lifecycle.node_stopped_at.remove(node_name);

    if let Some(metadata) = world.blend_relays.metadata(node_name)? {
        log_blend_relay_event(
            world,
            "blend_relay_configured",
            node_name,
            metadata.declared_addr,
            metadata.backend_addr,
            "setup",
        );
    }

    let node_final_dir = extract_child_dir_name(
        &world.lifecycle.scenario_base_dir,
        &runtime_dir_prefix,
        &final_dir_ignore_list,
    )
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;
    let node_runtime_dir = world
        .lifecycle
        .scenario_base_dir
        .join(node_final_dir.clone());
    world.chain.slots_per_epoch = runtime_info.slots_per_epoch;

    info!(
        target: TARGET,
        "Starting node `{node_name}` with runtime_dir='{}'",
        display_last_path_components(&node_runtime_dir, 4)
    );

    // `StartNodeOptions::with_persist_dir` currently creates a fresh runtime
    // directory for each launch. Seed that runtime directory and restart once
    // to effectively initialize from a named snapshot.
    let restored_node_snapshot = if let Some(node_snapshot) =
        world.snapshots.node_snapshot_on_startup.clone()
    {
        let stop_result = {
            let control = world.cluster.local_control()?;
            control
                .stop_node(&started_node_name)
                .await
                .inspect_err(|e| {
                    warn!(target: TARGET, "Step `{step}` error: {e}");
                })
        };
        stop_result?;
        world
            .lifecycle
            .node_stopped_at
            .insert(node_name.to_owned(), Instant::now());

        restore_node_state_from_snapshot(&node_snapshot, &node_runtime_dir).inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

        wait_for_node_restart_grace_period(world, node_name).await?;

        let restart_result = {
            let control = world.cluster.local_control()?;
            control
                .restart_node(&started_node_name)
                .await
                .inspect_err(|e| {
                    warn!(target: TARGET, "Step `{step}` error: {e}");
                })
        };
        restart_result?;
        world.lifecycle.node_stopped_at.remove(node_name);
        info!(
            target: TARGET,
            "Node {node_name} started from snapshot {}/{}",
            node_snapshot.name, node_snapshot.node
        );
        Some(node_snapshot)
    } else {
        None
    };

    world
        .cluster
        .node_peer_ids
        .insert(node_name.to_owned(), runtime_info.peer_id);

    let wallet_info = add_wallets(
        world,
        step,
        node_name,
        wallet_start_info,
        &client,
        &runtime_info.wallets,
        join_external_network,
    )
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    world
        .wallet_registry
        .wallet_info
        .extend(wallet_info.iter().map(|(k, v)| (k.clone(), v.clone())));

    world.nodes_info.insert(
        node_name.to_owned(),
        NodeInfo {
            name: node_name.to_owned(),
            runtime_name: started_node_name.clone(),
            client: client.clone(),
            chain_info: HashMap::default(),
            wallet_info,
            runtime_dir: node_runtime_dir,
            immediate_start,
        },
    );

    if let Some(node_snapshot) = restored_node_snapshot
        && let Some(snapshot_name) = world.snapshots.restore.extensions.clone()
    {
        restore_wallet_snapshot_if_present(
            &snapshot_name,
            &node_snapshot.node,
            node_name,
            &client,
            world,
        )
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    }

    // All nodes are required to be network ready responsive, and bootstrap nodes
    // must be `Mode::OnLine` for IBD of other peers to succeed
    if !immediate_start {
        let control = world.cluster.local_control()?;
        ensure_node_ready(
            control,
            &client,
            node_name,
            &started_node_name,
            is_bootstrap_node,
            world.startup.require_all_peers_mode_online_at_startup,
            join_external_network,
        )
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    }

    if world.snapshots.node_snapshot_on_startup.is_some() {
        match client.consensus_info().await {
            Ok(info) => {
                info!(
                    target: TARGET,
                    "Node `{node_name}` snapshot state - height: {}/{}, tip: {}, lib: {}",
                    info.cryptarchia_info.height,
                    info.cryptarchia_info.slot.into_inner(),
                    truncate_hash(&info.cryptarchia_info.tip.encode_hex::<String>(), 16),
                    truncate_hash(&info.cryptarchia_info.lib.encode_hex::<String>(), 16)
                );
            }
            Err(e) => {
                warn!(
                    target: TARGET,
                    "Node `{node_name}` failed to fetch post-start consensus after snapshot init: {e}"
                );
            }
        }
    }

    if let Some(tokio_console) = tokio_console_node {
        check_tokio_console_port(node_name, tokio_console.port);
    }

    Ok(())
}

/// Starts a local node and returns its TF name, API client and runtime
/// information.
///
/// Ordinary starts use the common app control and prepared runtime information.
/// Starts with Logos configuration patches require the typed Logos handle and
/// use runtime information captured after those patches are applied.
async fn launch_local_node(
    world: &CucumberWorld,
    node_name: &str,
    common_options: NodeLaunchOptions,
    logos_start_options: Option<StartNodeOptions<LbcEnv>>,
    effective_runtime_info: &OnceLock<NodeRuntimeInfo>,
) -> Result<(String, NodeHttpClient, NodeRuntimeInfo), StepError> {
    if let Some(mut options) = logos_start_options {
        let cluster = world
            .cluster
            .logos_cluster()
            .ok_or_else(|| StepError::LogicalError {
                message: "this scenario requires typed Logos configuration".into(),
            })?;

        options.common = common_options;
        let started = Box::pin(cluster.start_node_with(node_name, options)).await?;

        let runtime_info =
            effective_runtime_info
                .get()
                .cloned()
                .ok_or_else(|| StepError::LogicalError {
                    message: "Logos startup did not capture the effective configuration".into(),
                })?;

        Ok((started.name, started.client, runtime_info))
    } else {
        let control = world.cluster.local_control()?;
        let index = control.node_names().len();
        let runtime_info = world
            .cluster
            .node_runtime_info
            .get(index)
            .cloned()
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "no prepared runtime information for node index {index}; increase the cluster capacity"
                ),
            })?;

        let started = control.start_node_with(node_name, common_options).await?;
        let client = NodeHttpClient::from_url(started.access.api_base_url()?);

        Ok((started.name, client, runtime_info))
    }
}

fn check_tokio_console_port(node_name: &str, port: u16) {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(_) => info!(
            target: TARGET,
            "Tokio console endpoint for `{node_name}` is listening at port `{port}`, connect with \
            `tokio-console http://127.0.0.1:{port}`"
        ),
        Err(error) => warn!(
            target: TARGET,
            "Tokio console endpoint for `{node_name}` is not reachable at \
            `http://127.0.0.1:{port}`: {error}. Refer to the repo root `README.md -> Tokio task \
            profiling` for general instructions."
        ),
    }
}

/// Stop a node and leave it down.
///
/// Unlike [`restart_node`], which brings it back up and waits for readiness,
/// this leaves the node down, useful to exercise reconnect behavior while the
/// node is down.
pub async fn stop_node(world: &mut CucumberWorld, step: &str, node_name: &str) -> StepResult {
    let control = world.cluster.local_control()?;
    let started_node_name = world
        .resolve_node_runtime_name(node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    log_node_lifecycle_marker(world, "node_stop", node_name, "before").await;

    control
        .stop_node(&started_node_name)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    world
        .lifecycle
        .node_stopped_at
        .insert(node_name.to_owned(), Instant::now());

    log_node_lifecycle_marker(world, "node_stop", node_name, "after").await;

    info!(
        target: TARGET,
        "Stopped node `{node_name}` (runtime name `{started_node_name}`)"
    );
    Ok(())
}

pub async fn restart_node(world: &mut CucumberWorld, step: &str, node_name: &str) -> StepResult {
    if !world.lifecycle.node_stopped_at.contains_key(node_name) {
        stop_node(world, step, node_name).await?;
    }
    wait_for_node_restart_grace_period(world, node_name).await?;

    let control = world.cluster.local_control()?;
    let started_node_name = world
        .resolve_node_runtime_name(node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    log_node_lifecycle_marker(world, "node_restart", node_name, "before").await;

    control
        .restart_node(&started_node_name)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    world.lifecycle.node_stopped_at.remove(node_name);

    log_node_lifecycle_marker(world, "node_restart", node_name, "after").await;
    let client = world.resolve_node_http_client(node_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;
    ensure_node_ready(
        control,
        &client,
        node_name,
        &started_node_name,
        // TODO: Add `is_bootstrap_node` to world
        false,
        None,
        world.startup.join_external_network.unwrap_or_default(),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    info!(
        target: TARGET,
        "Restarted node `{node_name}` (runtime name `{started_node_name}`)"
    );

    Ok(())
}

async fn wait_for_node_restart_grace_period(world: &CucumberWorld, node_name: &str) -> StepResult {
    let stopped_at = world
        .lifecycle
        .node_stopped_at
        .get(node_name)
        .copied()
        .ok_or(StepError::LogicalError {
            message: format!("node `{node_name}` has no recorded stop time before restart"),
        })?;
    let elapsed = stopped_at.elapsed();
    if let Some(remaining) = NODE_RESTART_GRACE_PERIOD.checked_sub(elapsed) {
        info!(
            target: TARGET,
            node = node_name,
            ?remaining,
            "Waiting for node restart grace period"
        );
        sleep(remaining).await;
    }
    Ok(())
}

fn add_wallets(
    world: &CucumberWorld,
    step: &str,
    node_name: &str,
    wallet_start_info: &[WalletStartInfo],
    client: &NodeHttpClient,
    node_wallet_keys: &[NodeWalletKey],
    join_external_network: bool,
) -> Result<WalletInfoMap, StepError> {
    let wallet_info = compile_wallet_in_map(
        wallet_start_info,
        node_name,
        world,
        step,
        node_wallet_keys,
        join_external_network,
    )?;
    for (wallet_name, info) in &wallet_info {
        let wallet_type = match info.wallet_type.clone() {
            WalletType::User { .. } => "User",
            WalletType::Funding { .. } => "Funding",
        };
        info!(target: TARGET, "{wallet_type} wallet `{}/{node_name}` created: {}",
           wallet_name,
           format!("{}wallet/{}/balance", client.base_url(), info.public_key_hex())
        );
    }

    Ok(wallet_info)
}

struct StartupSettings {
    peer_selection: PeerSelection,
    ibd_peers: HashSet<PeerId>,
    is_bootstrap_node: bool,
    initial_peers_override: Option<Vec<Multiaddr>>,
    join_external_network: bool,
    user_config_overrides: Vec<ConfigOverride>,
    deployment_config_overrides: Vec<ConfigOverride>,
    deployment_settings_override: Option<DeploymentSettings>,
    manual_node_config_overrides: ManualNodeConfigOverrides,
    tokio_console_node: Option<TokioConsoleProfileNode>,
}

impl StartupSettings {
    fn logos_start_options(
        self,
        scenario_wallet_key_ids: HashSet<KeyId>,
        blend_relays: BlendRelayRegistry,
        node_name: String,
        effective_runtime_info: Arc<OnceLock<NodeRuntimeInfo>>,
    ) -> Result<Option<StartNodeOptions<LbcEnv>>, StepError> {
        if !self.join_external_network
            && self.deployment_settings_override.is_none()
            && self
                .manual_node_config_overrides
                .cryptarchia_security_param
                .is_none()
            && self
                .manual_node_config_overrides
                .prolonged_bootstrap_period
                .is_none()
            && self.initial_peers_override.is_none()
            && self.ibd_peers.is_empty()
            && self.user_config_overrides.is_empty()
            && self.deployment_config_overrides.is_empty()
            && self.tokio_console_node.is_none()
            && !blend_relays.is_enabled()?
        {
            return Ok(None);
        }

        Ok(Some(StartNodeOptions::default().create_patch(
            move |mut config: RunConfig| {
                prepare_config_patch(
                    &mut config,
                    self.join_external_network,
                    self.deployment_settings_override.as_ref(),
                    &self.manual_node_config_overrides,
                    self.initial_peers_override.as_ref(),
                    &self.ibd_peers,
                    &self.user_config_overrides,
                    &self.deployment_config_overrides,
                    self.tokio_console_node.as_ref(),
                    &scenario_wallet_key_ids,
                )?;

                let declared_blend_address =
                    config.user.blend.core.backend.listening_address.clone();
                blend_relays.configure_provider(
                    &node_name,
                    &mut config,
                    &declared_blend_address,
                )?;

                effective_runtime_info
                    .set(NodeRuntimeInfo::from_config(&config)?)
                    .map_err(|_| "Logos configuration was prepared more than once")?;

                Ok(config)
            },
        )))
    }
}

fn get_startup_settings(
    world: &CucumberWorld,
    initial_peers: &[String],
    node_name: &str,
) -> Result<StartupSettings, StepError> {
    let peer_selection = if initial_peers.is_empty() {
        PeerSelection::None
    } else {
        let named = initial_peers
            .iter()
            .map(|peer| world.resolve_node_runtime_name(peer))
            .collect::<Result<Vec<String>, StepError>>()?;
        PeerSelection::Named(named)
    };
    let mut ibd_peers = world.startup.ibd_peers_override.clone().unwrap_or_default();
    let populate_ibd_peers_from_initial_peers = world
        .startup
        .populate_ibd_peers_from_initial_peers
        .unwrap_or_default();
    if populate_ibd_peers_from_initial_peers {
        for peer in initial_peers {
            if let Some(peer_id) = world.cluster.node_peer_ids.get(peer) {
                ibd_peers.insert(*peer_id);
            }
        }
    }
    let is_bootstrap_node = initial_peers.is_empty();
    let initial_peers_override = world.startup.initial_peers_override.clone();
    let join_external_network = world.startup.join_external_network.unwrap_or_default();
    let deployment_settings_override = world
        .startup
        .deployment_config_override_path
        .clone()
        .map(|path| load_run_config(&path))
        .transpose()?;
    let user_config_overrides = world.startup.user_config_overrides.clone();
    let deployment_config_overrides = world.startup.deployment_config_overrides.clone();
    let tokio_console_node = world.tokio_console_profile.node(node_name).cloned();

    Ok(StartupSettings {
        peer_selection,
        ibd_peers,
        is_bootstrap_node,
        initial_peers_override,
        join_external_network,
        deployment_settings_override,
        manual_node_config_overrides: world.startup.manual_node_config_overrides.clone(),
        user_config_overrides,
        deployment_config_overrides,
        tokio_console_node,
    })
}

#[expect(clippy::too_many_arguments, reason = "all needed")]
fn prepare_config_patch(
    config: &mut RunConfig,
    join_external_network: bool,
    deployment_override: Option<&DeploymentSettings>,
    config_overrides: &ManualNodeConfigOverrides,
    initial_peers_override: Option<&Vec<Multiaddr>>,
    ibd_peers: &HashSet<PeerId>,
    user_config_overrides: &[ConfigOverride],
    deployment_config_overrides: &[ConfigOverride],
    tokio_console_node: Option<&TokioConsoleProfileNode>,
    scenario_wallet_key_ids: &HashSet<KeyId>,
) -> Result<(), StepError> {
    if join_external_network {
        config.deployment = deployment_override
            .cloned()
            .unwrap_or_else(DeploymentSettings::default);
    } else if let Some(deployment_override) = deployment_override {
        config.deployment = deployment_override.clone();
    }

    config_overrides.apply_to(config);

    if let Some(initial_peers) = &initial_peers_override {
        config
            .user
            .network
            .backend
            .initial_peers
            .clone_from(initial_peers);
    }
    config
        .user
        .cryptarchia
        .network
        .bootstrap
        .ibd
        .peers
        .clone_from(ibd_peers);
    if let Some(node) = &tokio_console_node {
        config.user.tracing.console = ConsoleLayer::Console(TokioConfig {
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: node.port,
            recording_path: node
                .record_raw
                .then(|| PathBuf::from("tokio-console-raw.jsonl")),
        });
    }

    apply_user_config_overrides(config, user_config_overrides)?;
    apply_deployment_config_overrides(config, deployment_config_overrides)?;
    if join_external_network {
        remove_external_scenario_wallet_keys(config, scenario_wallet_key_ids);
    }
    Ok(())
}

pub(super) fn wallet_account_key_id(account: &WalletAccount) -> KeyId {
    let key: Key = account.secret_key.clone().into();
    key_id_for_preload_backend(&key)
}

fn remove_external_scenario_wallet_keys(
    config: &mut RunConfig,
    scenario_wallet_key_ids: &HashSet<KeyId>,
) {
    remove_external_scenario_wallet_keys_from_maps(
        &mut config.user.wallet.known_keys,
        &mut config.user.kms.backend.keys,
        scenario_wallet_key_ids,
    );
}

pub(super) fn remove_external_scenario_wallet_keys_from_maps(
    known_keys: &mut HashMap<KeyId, lb_key_management_system_service::keys::ZkPublicKey>,
    kms_keys: &mut HashMap<KeyId, Key>,
    scenario_wallet_key_ids: &HashSet<KeyId>,
) {
    for key_id in scenario_wallet_key_ids {
        known_keys.remove(key_id);
        kms_keys.remove(key_id);
    }
}

fn load_run_config(path: &Path) -> Result<DeploymentSettings, StepError> {
    let text = fs::read_to_string(path).map_err(|e| StepError::LogicalError {
        message: format!("Failed to read '{}': {e}", path.display()),
    })?;
    serde_yaml::from_str::<DeploymentSettings>(&text).map_err(|e| StepError::LogicalError {
        message: format!("Failed to parse '{}': {e}", path.display()),
    })
}

// Ensure this node is ready, and achieved `Mode::OnLine` if it is a bootstrap
// node.
async fn ensure_node_ready(
    control: &dyn NodeControl,
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
    is_bootstrap_node: bool,
    require_all_peers_mode_online_at_startup: Option<Duration>,
    join_external_network: bool,
) -> StepResult {
    // General readiness check to ensure the node is responsive.
    let operation = format!("node '{started_node_name}' readiness");
    track_progress(&operation, Duration::from_secs(5), async {
        control
            .wait_node_ready(started_node_name)
            .await
            .map_err(|source| StepError::StepFail {
                message: format!(
                    "node '{started_node_name}' did not become ready after start: {source}"
                ),
            })
    })
    .await?;

    verify_reponsive_and_network_ready(client, node_name, started_node_name).await?;

    if !is_bootstrap_node && require_all_peers_mode_online_at_startup.is_none()
        || join_external_network
    {
        return Ok(());
    }

    verify_online(
        client,
        node_name,
        started_node_name,
        require_all_peers_mode_online_at_startup,
    )
    .await?;
    Ok(())
}

async fn verify_online(
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
    time_out: Option<Duration>,
) -> StepResult {
    let time_out = time_out.unwrap_or_else(|| Duration::from_mins(1));
    let start = Instant::now();
    let mut count = 0usize;
    loop {
        let mut mode_online = false;
        match client.consensus_info().await {
            Ok(val) => {
                if matches!(val.phase, PhaseTag::Following) {
                    mode_online = true;
                }
            }
            Err(e) if start.elapsed() < time_out => {
                if count.is_multiple_of(20) {
                    info!(
                        target: TARGET,
                        "Waiting for node `{node_name}/{started_node_name}` to be `Mode::OnLine` - \
                         elapsed: {:.2?} ({e})",
                        start.elapsed()
                    );
                }
            }
            Err(e) => {
                return Err(StepError::StepFail {
                    message: format!(
                        "Node `{node_name}/{started_node_name}` failed `Mode::OnLine` - elapsed \
                        {:.2?}: {e}",
                        start.elapsed()
                    ),
                });
            }
        }
        if mode_online {
            info!(
                target: TARGET,
                "Node `{node_name}/{started_node_name}` achieved `Mode::OnLine` and listen \
                addresses in {:.2?}",
                start.elapsed()
            );
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

/// Wait for all nodes to become responsive
pub async fn wait_all_nodes_responive(app: &LocalDeployment, time_out: Duration) -> StepResult {
    let readiness = app
        .runtime()
        .get::<Arc<dyn ClusterWaitHandle>>()
        .ok_or_else(|| StepError::LogicalError {
            message: "Local app does not provide cluster readiness".into(),
        })?;

    timeout(time_out, readiness.wait_network_ready())
        .await
        .map_err(|_| StepError::StepFail {
            message: format!("Not all nodes became responsive after {time_out:?}"),
        })?
        .map_err(|e| StepError::StepFail {
            message: format!("Failed to check all nodes ready: {e}"),
        })
}

async fn verify_reponsive_and_network_ready(
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
) -> StepResult {
    verify_reponsive_and_network_ready_with_timeout(
        client,
        node_name,
        started_node_name,
        Duration::from_mins(1),
    )
    .await
}

/// Wait for the node to be responsive and network ready, with a timeout.
#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub async fn verify_reponsive_and_network_ready_with_timeout(
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
    time_out: Duration,
) -> StepResult {
    let start = Instant::now();
    let mut count = 0usize;
    let mut can_provide_consensus_info;
    let mut is_network_ready;

    loop {
        can_provide_consensus_info = false;
        match client.consensus_info().await {
            Ok(_) => {
                can_provide_consensus_info = true;
            }
            Err(e) if start.elapsed() < time_out => {
                if count.is_multiple_of(20) {
                    info!(
                        target: TARGET,
                        "Waiting for node `{node_name}/{started_node_name}` to be responsive - \
                         elapsed: {:.2?} ({e})",
                        start.elapsed()
                    );
                }
            }
            Err(e) => {
                return Err(StepError::StepFail {
                    message: format!(
                        "Node `{node_name}/{started_node_name}` failed to be responsive - elapsed \
                        {:.2?}: {e}",
                        start.elapsed()
                    ),
                });
            }
        }
        is_network_ready = false;
        match client.network_info().await {
            Ok(val) => {
                is_network_ready = !val.listen_addresses.is_empty();
            }
            Err(e) if start.elapsed() < time_out => {
                if count.is_multiple_of(20) {
                    info!(
                        target: TARGET,
                        "Waiting for node `{node_name}/{started_node_name}` to be network ready - \
                        elapsed: {:.2?} ({e})",
                        start.elapsed()
                    );
                }
            }
            Err(e) => {
                return Err(StepError::StepFail {
                    message: format!(
                        "Node `{node_name}/{started_node_name}` failed to be network ready - elapsed \
                        {:.2?}: {e}",
                        start.elapsed()
                    ),
                });
            }
        }
        if can_provide_consensus_info && is_network_ready {
            info!(
                target: TARGET,
                "Node `{node_name}/{started_node_name}` is responsive and network ready in {:.2?}",
                start.elapsed()
            );
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

fn compile_wallet_in_map(
    wallet_start_info: &[WalletStartInfo],
    node_name: &str,
    world: &CucumberWorld,
    step: &str,
    node_wallet_keys: &[NodeWalletKey],
    join_external_network: bool,
) -> Result<WalletInfoMap, StepError> {
    let mut wallet_info: WalletInfoMap = HashMap::new();
    for wallet in wallet_start_info {
        let wallet_account = match world
            .wallet_registry
            .wallet_accounts
            .get(&wallet.account_index)
        {
            Some(wallet_account) => wallet_account.clone(),
            None if world.cluster.prepared_config.is_some() => {
                return Err(StepError::InvalidArgument {
                    message: format!(
                        "saved configuration has no wallet account {}",
                        wallet.account_index
                    ),
                });
            }
            None => {
                if join_external_network {
                    WalletAccount::random()
                        .map_err(|source| StepError::LogicalError {
                            message: format!(
                                "Step `{step}` error: Failed to derive random wallet account for index {}: {source}",
                                wallet.account_index
                            ),
                        })?
                } else {
                    WalletAccount::deterministic(
                        wallet.account_index as u64,
                        0,
                        true,
                    )
                        .map_err(|source| StepError::LogicalError {
                            message: format!(
                                "Step `{step}` error: Failed to derive deterministic wallet account for index {}: {source}",
                                wallet.account_index
                            ),
                        })?
                }
            }
        };

        wallet_info.insert(
            wallet.wallet_name.clone(),
            WalletInfo {
                wallet_name: wallet.wallet_name.clone(),
                node_name: node_name.to_owned(),
                wallet_type: WalletType::User { wallet_account },
            },
        );
    }

    let user_wallets_by_pk = world
        .wallet_registry
        .wallet_accounts
        .values()
        .map(|account| (account.public_key_hex(), account.label.clone()))
        .chain(
            world
                .wallet_registry
                .fee_state
                .wallet_account
                .iter()
                .map(|account| (account.public_key_hex(), account.label.clone())),
        )
        .collect::<HashMap<_, _>>();
    let mut generic_key_index = 0usize;

    for node_wallet_key in node_wallet_keys.iter().cloned() {
        if let Some(user_wallet_name) = user_wallets_by_pk.get(&node_wallet_key.wallet_pk) {
            if node_wallet_key.role != NodeWalletKeyRole::General {
                return Err(StepError::LogicalError {
                    message: format!(
                        "Scenario wallet `{user_wallet_name}` public key conflicts with the \
                         {role:?} key owned by `{node_name}`",
                        role = node_wallet_key.role,
                    ),
                });
            }
            info!(
                target: TARGET,
                "Scenario wallet `{user_wallet_name}` is registered with `{node_name}`; \
                 excluding its public key from node-wallet aliases"
            );
            continue;
        }

        let wallet_name = node_wallet_name(node_name, &node_wallet_key, &mut generic_key_index);
        wallet_info.insert(
            wallet_name.clone(),
            WalletInfo {
                wallet_name,
                node_name: node_name.to_owned(),
                wallet_type: WalletType::Funding {
                    key: node_wallet_key,
                },
            },
        );
    }

    Ok(wallet_info)
}

pub(super) fn node_wallet_name(
    node_name: &str,
    key: &NodeWalletKey,
    generic_key_index: &mut usize,
) -> String {
    let role = match key.role {
        NodeWalletKeyRole::Funding => "FUNDING".to_owned(),
        NodeWalletKeyRole::VoucherMaster => "VOUCHER_MASTER".to_owned(),
        NodeWalletKeyRole::BlendZk => "BLEND_ZK".to_owned(),
        NodeWalletKeyRole::General => {
            *generic_key_index += 1;
            format!("GENERAL_{}", *generic_key_index)
        }
    };
    format!("{node_name}_WALLET_{role}")
}
