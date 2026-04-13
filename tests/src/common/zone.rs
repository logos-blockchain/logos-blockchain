use std::{
    collections::HashSet,
    num::NonZero,
    path::{Path, PathBuf},
    time::Duration,
};

use futures::StreamExt as _;
use lb_common_http_client::CommonHttpClient;
use lb_core::mantle::{Transaction as _, ops::channel::ChannelId};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519PublicKey};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, LbcLocalDeployer, LbcManualCluster, NodeHttpClient, TopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
    internal::DeploymentPlan,
};
use lb_utils::math::NonNegativeRatio;
use lb_zone_sdk::{
    ZoneMessage,
    adapter::NodeHttpClient as ZoneNodeHttpClient,
    indexer::ZoneIndexer,
    sequencer::{InscriptionId, PublishResult, SequencerConfig, SequencerHandle},
};
use rand::{Rng as _, thread_rng};
use reqwest::Url;
use testing_framework_core::scenario::{DynError, StartNodeOptions, StartedNode};
use tokio::time::{sleep, timeout};
use tracing::warn;

use crate::{
    common::{
        chain::wait_for_transactions_inclusion, manual_cluster::ensure_local_node_binary_env,
    },
    cucumber::utils::{extract_child_dir_name, matching_child_dirs},
};

#[derive(Debug, thiserror::Error)]
pub enum ZoneTestError {
    #[error("failed to build zone deployment: {message}")]
    BuildDeployment { message: String },
    #[error("failed to start zone node: {message}")]
    StartNode { message: String },
    #[error("failed to resolve zone runtime dir: {message}")]
    RuntimeDir { message: String },
    #[error("zone network did not become ready: {message}")]
    NetworkReady { message: String },
    #[error("timed out waiting for zone sequencer to accept a publish request")]
    PublishTimeout,
    #[error("zone indexer request failed: {message}")]
    Indexer { message: String },
    #[error("timed out waiting for zone indexer to return all messages")]
    IndexerTimeout,
    #[error("timed out waiting for zone transactions to appear on the canonical chain")]
    InclusionTimeout,
    #[error("failed to fetch consensus info while checking finalized transactions: {message}")]
    Consensus { message: String },
    #[error("failed to fetch block while checking finalized transactions: {message}")]
    Block { message: String },
    #[error("timed out waiting for zone transactions to finalize")]
    FinalizationTimeout,
}

pub struct ZoneClusterTemplate {
    pub cluster: LbcManualCluster,
    pub channel_signing_key: Ed25519Key,
}

pub struct StartedZoneNode {
    pub started_node: StartedNode<LbcEnv>,
    pub runtime_dir: PathBuf,
}

pub fn build_zone_cluster(
    scenario_base_dir: PathBuf,
) -> Result<ZoneClusterTemplate, ZoneTestError> {
    ensure_local_node_binary_env();

    let deployment = build_zone_deployment(scenario_base_dir)?;
    let channel_signing_key = deployment.nodes()[0].general.blend_config.1.clone();
    let cluster = LbcLocalDeployer::new().manual_cluster_from_descriptors(deployment);

    Ok(ZoneClusterTemplate {
        cluster,
        channel_signing_key,
    })
}

pub async fn start_zone_node(
    cluster: &LbcManualCluster,
    scenario_base_dir: &Path,
) -> Result<StartedZoneNode, ZoneTestError> {
    let node_name = "0";
    let persist_dir = scenario_base_dir.join("node-0");

    let runtime_dir_prefix = format!(
        "{}_",
        persist_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("node-0")
    );
    let ignore_list = matching_child_dirs(&persist_dir, &runtime_dir_prefix);

    let started_node = Box::pin(
        cluster.start_node_with(
            node_name,
            StartNodeOptions::default()
                .with_persist_dir(persist_dir)
                .create_patch(fast_zone_config_patch),
        ),
    )
    .await
    .map_err(|error| ZoneTestError::StartNode {
        message: error.to_string(),
    })?;

    let runtime_dir_name =
        extract_child_dir_name(scenario_base_dir, &runtime_dir_prefix, &ignore_list).map_err(
            |error| ZoneTestError::RuntimeDir {
                message: error.to_string(),
            },
        )?;

    Ok(StartedZoneNode {
        started_node,
        runtime_dir: scenario_base_dir.join(runtime_dir_name),
    })
}

pub async fn wait_for_zone_network_ready(cluster: &LbcManualCluster) -> Result<(), ZoneTestError> {
    cluster
        .wait_network_ready()
        .await
        .map_err(|error| ZoneTestError::NetworkReady {
            message: error.to_string(),
        })
}

#[must_use]
pub fn channel_id_from_key(key: &Ed25519Key) -> ChannelId {
    ChannelId::from(key.public_key().to_bytes())
}

#[must_use]
pub fn sequencer_config() -> SequencerConfig {
    SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    }
}

#[must_use]
pub fn random_second_public_key() -> Ed25519PublicKey {
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    Ed25519Key::from_bytes(&key_bytes).public_key()
}

pub async fn publish_message_with_retry(
    sequencer: &SequencerHandle<ZoneNodeHttpClient>,
    data: &[u8],
    publish_start: std::time::Instant,
    publish_timeout: Duration,
) -> Result<PublishResult, ZoneTestError> {
    loop {
        if publish_start.elapsed() > publish_timeout {
            return Err(ZoneTestError::PublishTimeout);
        }

        match sequencer.publish_message(data.to_vec()).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                warn!(error = %error, "Zone sequencer publish failed, retrying");

                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

pub async fn collect_indexed_messages(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected_messages: &[Vec<u8>],
    duration: Duration,
) -> Result<Vec<Vec<u8>>, ZoneTestError> {
    let expected: HashSet<Vec<u8>> = expected_messages.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut ordered: Vec<Vec<u8>> = Vec::new();
    let mut cursor = None;

    timeout(duration, async {
        while seen != expected {
            let stream =
                indexer
                    .next_messages(cursor)
                    .await
                    .map_err(|error| ZoneTestError::Indexer {
                        message: error.to_string(),
                    })?;
            futures::pin_mut!(stream);

            while let Some((message, slot)) = stream.next().await {
                let ZoneMessage::Block(block) = message else {
                    continue;
                };

                if expected.contains(&block.data) && seen.insert(block.data.clone()) {
                    ordered.push(block.data.clone());
                }

                cursor = Some((block.id, slot));
            }

            if seen != expected {
                sleep(Duration::from_millis(500)).await;
            }
        }

        Ok::<(), ZoneTestError>(())
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)??;

    Ok(ordered)
}

pub async fn collect_indexed_messages_exactly_once(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected_messages: &[Vec<u8>],
    duration: Duration,
) -> Result<Vec<Vec<u8>>, ZoneTestError> {
    let expected: HashSet<Vec<u8>> = expected_messages.iter().cloned().collect();

    timeout(duration, async {
        loop {
            let mut ordered = Vec::new();
            let mut cursor = None;

            loop {
                let stream = indexer.next_messages(cursor).await.map_err(|error| {
                    ZoneTestError::Indexer {
                        message: error.to_string(),
                    }
                })?;
                futures::pin_mut!(stream);
                let mut saw_message = false;

                while let Some((message, slot)) = stream.next().await {
                    let ZoneMessage::Block(block) = message else {
                        continue;
                    };

                    saw_message = true;
                    cursor = Some((block.id, slot));

                    if expected.contains(&block.data) {
                        ordered.push(block.data);
                    }
                }

                if !saw_message {
                    break;
                }
            }

            if ordered == expected_messages {
                return Ok(ordered);
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

pub async fn ensure_zone_transactions_included(
    client: &NodeHttpClient,
    tx_hashes: &[InscriptionId],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let included = wait_for_transactions_inclusion(client, tx_hashes, duration).await;

    if included {
        return Ok(());
    }

    Err(ZoneTestError::InclusionTimeout)
}

pub async fn wait_for_transactions_finalized(
    node_url: Url,
    tx_hashes: &[InscriptionId],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let client = CommonHttpClient::new(None);
    let expected: HashSet<_> = tx_hashes.iter().copied().collect();

    timeout(duration, async {
        loop {
            let info = client
                .consensus_info(node_url.clone())
                .await
                .map_err(|error| ZoneTestError::Consensus {
                    message: error.to_string(),
                })?;

            let mut found = HashSet::new();
            let mut current = info.lib;

            while let Some(block) =
                client
                    .get_block(node_url.clone(), current)
                    .await
                    .map_err(|error| ZoneTestError::Block {
                        message: error.to_string(),
                    })?
            {
                for tx in block.transactions() {
                    let hash = tx.mantle_tx.hash();
                    if expected.contains(&hash) {
                        found.insert(hash);
                    }
                }

                current = block.header().parent_block();
            }

            if found == expected {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::FinalizationTimeout)?
}

fn build_zone_deployment(scenario_base_dir: PathBuf) -> Result<DeploymentPlan, ZoneTestError> {
    DeploymentBuilder::new(TopologyConfig::with_node_numbers(1))
        .with_wallet_config(WalletConfig::new(build_zone_wallet_accounts()))
        .scenario_base_dir(scenario_base_dir)
        .build()
        .map_err(|error| ZoneTestError::BuildDeployment {
            message: error.to_string(),
        })
}

fn build_zone_wallet_accounts() -> Vec<WalletAccount> {
    (0..4)
        .map(|index| {
            WalletAccount::deterministic(index, 10_000, false).expect("wallet account should build")
        })
        .collect()
}

fn fast_zone_config_patch(mut config: RunConfig) -> Result<RunConfig, DynError> {
    if config.user.api.backend.listen_address.port() == 0 {
        return Err("zone test config patch requires a non-zero API port".into());
    }

    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());
    config
        .user
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;
    config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
    Ok(config)
}
