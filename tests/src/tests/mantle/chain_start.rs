use std::{num::NonZero, path::PathBuf, time::Duration};

use lb_chain_service::PhaseTag;
use lb_node::config::{RunConfig, cryptarchia::deployment::EpochConfig};
use lb_testing_framework::{
    DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::deployment::NodeBinaryProfile, ensure_node_binary_built,
};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::manual_cluster::{
        ManualNodeLayout, start_local_manual_cluster_with_layout, wait_for_nodes_height,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::DynError;
use time::OffsetDateTime;
use tokio::time::sleep;

const NODE_COUNT: usize = 1;
const MODE_TIMEOUT_SECS: u64 = 60;

#[tokio::test]
async fn delayed_chain_start() {
    // Resolve/build the node binary up front so the genesis-time countdown doesn't
    // need to account for compilation time
    ensure_node_binary_built(&NodeBinaryProfile::default())
        .await
        .expect("node binary should build or resolve");
    let genesis_time = OffsetDateTime::now_utc() + Duration::from_secs(30);
    let (_base, nodes) = start_local_manual_cluster_with_layout(
        "delayed-chain-start",
        "mantle-chain-start",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_test_context(Some("delayed_chain_start".to_owned())),
        )
        .with_genesis_time(
            genesis_time
                .try_into()
                .expect("genesis time should fit in GenesisTime"),
        ),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok(test_config(config)),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await;

    let node0 = &nodes[0];

    let info =
        wait_for_consensus_mode(&node0.client, Duration::from_secs(MODE_TIMEOUT_SECS), |i| {
            i.phase == PhaseTag::AwaitingGenesisTime
        })
        .await
        .expect("Failed to get AwaitingGenesisTime phase");

    assert_eq!(info.phase, PhaseTag::AwaitingGenesisTime);

    let info =
        wait_for_consensus_mode(&node0.client, Duration::from_secs(MODE_TIMEOUT_SECS), |i| {
            matches!(i.phase, PhaseTag::Following)
        })
        .await
        .expect("Failed to reach the Following phase");

    assert_eq!(info.phase, PhaseTag::Following);

    wait_for_nodes_height(&[&node0.client], 1, Duration::from_secs(500)).await;
}

async fn wait_for_consensus_mode<F>(
    client: &NodeHttpClient,
    timeout: Duration,
    mut predicate: F,
) -> Result<lb_chain_service::ChainServiceInfo, DynError>
where
    F: FnMut(&lb_chain_service::ChainServiceInfo) -> bool,
{
    let start = tokio::time::Instant::now();

    loop {
        if let Ok(info) = client.consensus_info().await
            && predicate(&info)
        {
            return Ok(info);
        }

        if start.elapsed() > timeout {
            return Err("Timed out waiting for consensus mode".into());
        }

        sleep(Duration::from_millis(500)).await;
    }
}

fn test_config(mut config: RunConfig) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    config.deployment.cryptarchia.security_param = NonZero::new(2).unwrap();
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 10.try_into().unwrap());

    config
}
