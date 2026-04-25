use std::{collections::HashMap, hash::BuildHasher, time::Duration};

use lb_libp2p::{Multiaddr, PeerId, Protocol};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, LbcLocalDeployer, NodeHttpClient, TopologyConfig,
    configs::wallet::WalletAccount, internal::DeploymentPlan,
};
use testing_framework_core::scenario::{StartNodeOptions, StartedNode};
use tokio::time::{Instant, sleep};
use tracing::warn;

use crate::cucumber::{
    error::{StepError, StepResult},
    fee_reserve::create_scenario_fee_wallet_account,
    steps::TARGET,
    world::{
        CucumberWorld, ManualClusterKind, ManualClusterSpec, NodeInfo, WalletInfo, WalletType,
    },
};

fn apply_blend_core_nodes(
    world: &CucumberWorld,
    mut config: TopologyConfig,
    nodes_count: usize,
) -> Result<TopologyConfig, StepError> {
    let blend_core_nodes = world.blend_core_nodes.unwrap_or(nodes_count);

    if blend_core_nodes > nodes_count {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Blend provider count ({blend_core_nodes}) must be <= cluster capacity ({nodes_count})"
            ),
        });
    }

    config = config.with_blend_core_nodes(blend_core_nodes);

    Ok(config)
}

pub fn build_manual_cluster_deployment(
    world: &mut CucumberWorld,
    nodes_count: usize,
) -> Result<DeploymentPlan, StepError> {
    let config = TopologyConfig::with_node_numbers(nodes_count)
        .with_allow_multiple_genesis_tokens(true)
        .with_allow_zero_value_genesis_tokens(true)
        .with_test_context(world.test_context.clone());
    let mut config = apply_blend_core_nodes(world, config, nodes_count)?;

    for genesis_token in &world.genesis_tokens {
        let wallet_account = WalletAccount::deterministic(
            genesis_token.account_index as u64,
            genesis_token.token_amount,
            true,
        )?;

        world
            .wallet_accounts
            .insert(genesis_token.account_index, wallet_account.clone());
        for _ in 0..genesis_token.token_count {
            config.wallet_config.accounts.push(wallet_account.clone());
        }
    }

    world.fee_state.wallet_account = match world.fee_state.sponsored_genesis_account {
        Some(sponsored_genesis_account) => {
            let scenario_fee_wallet_account =
                create_scenario_fee_wallet_account(sponsored_genesis_account.token_value)?;

            for _ in 0..sponsored_genesis_account.token_count.get() {
                config
                    .wallet_config
                    .accounts
                    .push(scenario_fee_wallet_account.clone());
            }
            Some(scenario_fee_wallet_account)
        }
        None => None,
    };

    let deployment =
        DeploymentBuilder::new(config)
            .build()
            .map_err(|e| StepError::LogicalError {
                message: format!("failed to build manual cluster: {e}"),
            })?;

    if let Some(genesis_tx) = deployment.config.genesis_tx.clone() {
        world.genesis_block_utxos =
            crate::cucumber::steps::manual_nodes::utils::genesis_block_utxos(&genesis_tx);
    }

    Ok(deployment)
}

pub fn install_local_manual_cluster(
    world: &mut CucumberWorld,
    spec: ManualClusterSpec,
) -> Result<(), StepError> {
    let deployment = build_manual_cluster_from_spec(world, spec)?;
    let deployer = LbcLocalDeployer::new();
    let cluster = deployer.manual_cluster_from_descriptors(deployment);

    world.local_cluster = Some(cluster);
    world.k8s_manual_cluster = None;
    world.manual_cluster_spec = Some(spec);

    Ok(())
}

fn build_devnet_manual_cluster_deployment(
    world: &mut CucumberWorld,
    nodes_count: usize,
) -> Result<DeploymentPlan, StepError> {
    // For devnet runs we do not allocate genesis tokens/accounts here.
    // Wallet keys are derived later, and node startup may switch deployment
    // settings, so locally generated genesis outputs are not meaningful for
    // wallet tracking.
    world.genesis_block_utxos.clear();
    world.wallet_accounts.clear();

    let config = TopologyConfig::with_node_numbers(nodes_count)
        .with_allow_multiple_genesis_tokens(true)
        .with_allow_zero_value_genesis_tokens(true)
        .with_test_context(world.test_context.clone());
    let config = apply_blend_core_nodes(world, config, nodes_count)?;

    DeploymentBuilder::new(config)
        .build()
        .map_err(|e| StepError::LogicalError {
            message: format!("failed to build devnet manual cluster: {e}"),
        })
}

fn build_manual_cluster_from_spec(
    world: &mut CucumberWorld,
    spec: ManualClusterSpec,
) -> Result<DeploymentPlan, StepError> {
    match spec.kind {
        ManualClusterKind::Generated => build_manual_cluster_deployment(world, spec.capacity),
        ManualClusterKind::Devnet => build_devnet_manual_cluster_deployment(world, spec.capacity),
    }
}

pub fn rebuild_pending_local_manual_cluster(world: &mut CucumberWorld) -> StepResult {
    if world.nodes_info.is_empty() {
        if let Some(spec) = world.manual_cluster_spec {
            return install_local_manual_cluster(world, spec);
        }

        return Ok(());
    }

    Err(StepError::LogicalError {
        message: "cannot change manual cluster deployment shape after nodes have started".into(),
    })
}

pub fn stop_active_manual_cluster(world: &CucumberWorld) -> StepResult {
    if let Some(cluster) = world.local_cluster.as_ref() {
        cluster.stop_all();
        return Ok(());
    }

    if let Some(cluster) = world.k8s_manual_cluster.as_ref() {
        cluster.stop_all();
        return Ok(());
    }

    Err(StepError::LogicalError {
        message: "No manual cluster available".into(),
    })
}

pub async fn start_manual_node(
    world: &CucumberWorld,
    node_name: &str,
    options: StartNodeOptions<LbcEnv>,
) -> Result<StartedNode<LbcEnv>, StepError> {
    if let Some(cluster) = world.local_cluster.as_ref() {
        return Box::pin(cluster.start_node_with(node_name, options))
            .await
            .map_err(|e| StepError::LogicalError {
                message: format!("failed to start node '{node_name}': {e}"),
            });
    }

    if let Some(cluster) = world.k8s_manual_cluster.as_ref() {
        return Box::pin(cluster.start_node_with(node_name, options))
            .await
            .map_err(|e| StepError::LogicalError {
                message: format!("failed to start node '{node_name}': {e}"),
            });
    }

    Err(StepError::LogicalError {
        message: "No manual cluster available".into(),
    })
}

pub async fn wait_manual_node_ready(world: &CucumberWorld, node_name: &str) -> StepResult {
    if let Some(cluster) = world.local_cluster.as_ref() {
        return cluster
            .wait_node_ready(node_name)
            .await
            .map_err(|e| StepError::LogicalError {
                message: format!("node '{node_name}' did not become ready: {e}"),
            });
    }

    if let Some(cluster) = world.k8s_manual_cluster.as_ref() {
        return cluster
            .wait_node_ready(node_name)
            .await
            .map_err(|e| StepError::LogicalError {
                message: format!("node '{node_name}' did not become ready: {e}"),
            });
    }

    Err(StepError::LogicalError {
        message: "No manual cluster available".into(),
    })
}

pub fn manual_node_client(
    world: &CucumberWorld,
    node_name: &str,
) -> Result<NodeHttpClient, StepError> {
    if let Some(cluster) = world.local_cluster.as_ref() {
        return cluster
            .node_client(node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("missing client for node '{node_name}'"),
            });
    }

    if let Some(cluster) = world.k8s_manual_cluster.as_ref() {
        return cluster
            .node_client(node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("missing client for node '{node_name}'"),
            });
    }

    Err(StepError::LogicalError {
        message: "No manual cluster available".into(),
    })
}

pub async fn assert_manual_node_has_peers(
    world: &CucumberWorld,
    node_name: &str,
    min_peers: usize,
    timeout_secs: u64,
) -> StepResult {
    let runtime_node_name = world
        .resolve_node_runtime_name(node_name)
        .unwrap_or_else(|_| node_name.to_owned());
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        let client = manual_node_client(world, &runtime_node_name)?;
        let network = client.network_info().await?;
        if network.n_peers >= min_peers {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(StepError::StepFail {
                message: format!(
                    "node '{node_name}' did not reach {min_peers} peers in {timeout_secs}s \
                    (peers={}, connections={}, pending={})",
                    network.n_peers, network.n_connections, network.n_pending_connections
                ),
            });
        }

        warn!(
            target: TARGET,
            "waiting for node '{node_name}' to reach {min_peers} peers; current peers={} connections={} pending={}",
            network.n_peers,
            network.n_connections,
            network.n_pending_connections
        );
        sleep(Duration::from_secs(1)).await;
    }
}

pub async fn connect_manual_node_to_node(
    world: &CucumberWorld,
    source_node_name: &str,
    target_node_name: &str,
) -> StepResult {
    let source_client = world.resolve_node_http_client(source_node_name)?;
    let target_client = world.resolve_node_http_client(target_node_name)?;
    let target_network = target_client.network_info().await?;
    let target_addr = compose_dial_addr(
        target_network.listen_addresses.first(),
        target_network.peer_id,
    )
    .ok_or_else(|| StepError::LogicalError {
        message: format!("node '{target_node_name}' has no listen address to dial"),
    })?;

    source_client
        .dial_peer(target_addr)
        .await
        .map(|_| ())
        .map_err(|error| StepError::LogicalError {
            message: format!(
                "failed to connect node '{source_node_name}' to node '{target_node_name}': {error}"
            ),
        })
}

fn compose_dial_addr(listen_addr: Option<&Multiaddr>, peer_id: PeerId) -> Option<Multiaddr> {
    let mut addr = listen_addr?.clone();
    let has_peer_id = addr
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2p(_)));

    if !has_peer_id {
        addr.push(Protocol::P2p(peer_id));
    }

    Some(addr)
}

pub fn build_user_wallets(
    world: &CucumberWorld,
    node_name: &str,
    wallet_start_info: &[crate::cucumber::steps::manual_nodes::utils::WalletStartInfo],
) -> Result<HashMap<String, WalletInfo>, StepError> {
    let mut wallet_info = HashMap::new();
    for wallet in wallet_start_info {
        let wallet_account = match world.wallet_accounts.get(&wallet.account_index) {
            Some(wallet_account) => wallet_account.clone(),
            None => WalletAccount::deterministic(wallet.account_index as u64, 0, true).map_err(
                |source| StepError::LogicalError {
                    message: format!(
                        "failed to derive deterministic wallet account for index {}: {source}",
                        wallet.account_index,
                    ),
                },
            )?,
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

    Ok(wallet_info)
}

pub fn insert_started_node_info<S: BuildHasher>(
    world: &mut CucumberWorld,
    logical_node_name: &str,
    started_node: StartedNode<LbcEnv>,
    wallet_info: HashMap<String, WalletInfo, S>,
) {
    let wallet_info: HashMap<String, WalletInfo> = wallet_info.into_iter().collect();

    world
        .wallet_info
        .extend(wallet_info.iter().map(|(k, v)| (k.clone(), v.clone())));

    world.nodes_info.insert(
        logical_node_name.to_owned(),
        NodeInfo {
            name: logical_node_name.to_owned(),
            started_node,
            run_config: None,
            chain_info: HashMap::new(),
            wallet_info,
            runtime_dir: std::path::PathBuf::new(),
            immediate_start: world.network_immediate_start(logical_node_name),
        },
    );
}
