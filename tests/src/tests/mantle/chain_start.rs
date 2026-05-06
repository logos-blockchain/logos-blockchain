use std::{num::NonZero, time::Duration};

use lb_chain_service::{ChainServiceMode, State};
use lb_core::{
    block::genesis::GenesisBlockBuilder,
    mantle::{GenesisTx as _, ops::channel::inscribe::InscriptionOp},
};
use lb_node::config::{RunConfig, cryptarchia::deployment::EpochConfig};
use lb_testing_framework::{DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::common::manual_cluster::{
    ManualNodeLayout, start_local_manual_cluster_with_layout, wait_for_nodes_height,
};
use testing_framework_core::scenario::DynError;
use time::OffsetDateTime;
use tokio::time::sleep;

const NODE_COUNT: usize = 1;
const MODE_TIMEOUT_SECS: u64 = 60;

#[tokio::test]
async fn delayed_chain_start() {
    let genesis_time = OffsetDateTime::now_utc() + Duration::from_secs(30);
    let (_base, nodes) = start_local_manual_cluster_with_layout(
        "delayed-chain-start",
        "mantle-chain-start",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_test_context(Some("delayed_chain_start".to_owned())),
        ),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        move |config| Ok(test_config(config, genesis_time)),
    )
    .await;

    let node0 = &nodes[0];

    let info =
        wait_for_consensus_mode(&node0.client, Duration::from_secs(MODE_TIMEOUT_SECS), |i| {
            i.mode == ChainServiceMode::AwaitingStart
        })
        .await
        .expect("Failed to get AwaitingStart mode");

    assert_eq!(info.mode, ChainServiceMode::AwaitingStart);

    let info =
        wait_for_consensus_mode(&node0.client, Duration::from_secs(MODE_TIMEOUT_SECS), |i| {
            matches!(i.mode, ChainServiceMode::Started(State::Online))
        })
        .await
        .expect("Failed to reach Started(State::Online)");

    assert_eq!(info.mode, ChainServiceMode::Started(State::Online));

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

fn test_config(mut config: RunConfig, genesis_time: OffsetDateTime) -> RunConfig {
    let genesis_tx = config.deployment.cryptarchia.genesis_block.genesis_tx();

    let mut cryptarchia_parameter = genesis_tx.cryptarchia_parameter();
    cryptarchia_parameter.genesis_time = genesis_time;

    let inscription = InscriptionOp {
        inscription: cryptarchia_parameter.encode(),
        ..genesis_tx.genesis_inscription().clone()
    };

    config.deployment.cryptarchia.genesis_block = GenesisBlockBuilder::new()
        .add_notes(genesis_tx.genesis_transfer().outputs.iter().copied())
        .set_inscription(inscription)
        .build()
        .expect("Failed to build genesis block");

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
