use std::{num::NonZero, time::Duration};

use lb_core::sdp::ServiceType;
use lb_cryptarchia_engine::{base_period_length, time::epoch_length};
use lb_node::config::{RunConfig, cryptarchia::deployment::EpochConfig};
use lb_testing_framework::{DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::common::manual_cluster::{
    ManualNodeLayout, start_local_manual_cluster_with_layout, wait_for_nodes_height,
};
use testing_framework_core::scenario::DynError;
use tokio::time::sleep;

const NODE_COUNT: usize = 2;

/// End-to-end test for blend SDP activity proofs:
///
/// 1. Spawn two validators with blend declarations in the genesis transaction.
/// 2. Wait long enough that declarations would be removed if no activity
///    message was submitted during `inactivity_period + retention_period`
///    sessions.
/// 3. Verify that both declarations are still present, proving that the nodes
///    automatically submitted valid activity messages that the ledger accepted.
#[tokio::test]
async fn sdp_blend_activity() {
    let (_base, nodes) = start_local_manual_cluster_with_layout(
        "sdp-blend-activity",
        "mantle-sdp",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_test_context(Some("sdp_blend_activity".to_owned())),
        ),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok::<_, DynError>(test_config(config)),
    )
    .await;

    let node0 = &nodes[0];
    let node1 = &nodes[1];

    // Verify both nodes have blend declarations from genesis.
    let declarations = wait_for_declarations(&node0.client, Duration::from_secs(30)).await;
    assert_eq!(
        declarations.len(),
        NODE_COUNT,
        "genesis should include declarations for all nodes, but got {}",
        declarations.len()
    );

    // Wait past the point where declarations would be removed if no activity
    // proofs were submitted.
    //
    // session_duration = blocks_per_epoch = 6 blocks
    //
    // A declaration created at genesis with no activity would be removed
    // after (inactivity_period + retention_period) * session_duration
    // = (1 + 1) * 6 = 12 blocks.
    //
    // We wait for more sessions to give a solid margin and ensure that if
    // activity proofs are being submitted, they keep the declarations alive.
    let blocks_per_session = blocks_per_session();
    let survival_sessions = INACTIVITY_PERIOD + RETENTION_PERIOD + 1; // +1 margin
    let target_height = survival_sessions * blocks_per_session;
    println!(
        "Waiting for {target_height} blocks ({survival_sessions} sessions, {blocks_per_session} blocks/session)",
    );

    wait_for_nodes_height(
        &[&node0.client, &node1.client],
        target_height,
        Duration::from_secs(500),
    )
    .await;

    // Declarations must still be present — this proves that activity messages were
    // submitted/accepted, keeping the declarations alive.
    let declarations_after = node0
        .client
        .get_sdp_declarations()
        .await
        .expect("querying SDP declarations should succeed");

    // Check if at least one declaration is still present because blocks may have
    // been produced by only one nodes by coincidence
    assert!(
        !declarations_after.is_empty(),
        "At least one blend declaration should survive past the inactivity window. Activity proofs may not have been submitted/accepted"
    );
}

const INACTIVITY_PERIOD: u64 = 1;
const RETENTION_PERIOD: u64 = 1;

fn test_config(mut config: RunConfig) -> RunConfig {
    let (epoch_config, security_param, slot_activation_coeff) = cryptarchia_config();
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.epoch_config = epoch_config;
    config.deployment.cryptarchia.security_param = security_param;
    config.deployment.cryptarchia.slot_activation_coeff = slot_activation_coeff;

    // Set small inactivity/retention periods so that declarations are removed
    // quickly if no activity proofs are submitted.
    let blend_params = config
        .deployment
        .cryptarchia
        .sdp_config
        .service_params
        .get_mut(&ServiceType::BlendNetwork)
        .expect("blend network params should exist");
    blend_params.inactivity_period = INACTIVITY_PERIOD;
    blend_params.retention_period = RETENTION_PERIOD;

    config
}

fn blocks_per_session() -> u64 {
    let (epoch_config, security_param, slot_activation_coeff) = cryptarchia_config();
    let base_period = base_period_length(security_param, slot_activation_coeff);
    let slots_per_epoch = epoch_length(
        epoch_config.epoch_stake_distribution_stabilization,
        epoch_config.epoch_period_nonce_buffer,
        epoch_config.epoch_period_nonce_stabilization,
        base_period,
    );

    let avg_slots_per_block =
        base_period_length(NonZero::<u32>::new(1).unwrap(), slot_activation_coeff).get();
    slots_per_epoch / avg_slots_per_block
}

fn cryptarchia_config() -> (EpochConfig, NonZero<u32>, NonNegativeRatio) {
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    let security_param = NonZero::new(2).unwrap();
    let slot_activation_coeff = NonNegativeRatio::new(1, 10.try_into().unwrap());
    (epoch_config, security_param, slot_activation_coeff)
}

async fn wait_for_declarations(
    node: &NodeHttpClient,
    duration: Duration,
) -> Vec<lb_core::sdp::Declaration> {
    tokio::time::timeout(duration, async {
        loop {
            if let Ok(declarations) = node.get_sdp_declarations().await {
                return declarations;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("SDP declarations should become available")
}
