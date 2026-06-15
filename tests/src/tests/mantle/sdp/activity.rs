use std::{
    collections::HashMap,
    num::NonZero,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use lb_chain_service::Epoch;
use lb_core::sdp::{Declaration, NumberOfEpochs, ProviderId, ServiceType};
use lb_node::config::{RunConfig, cryptarchia::deployment::EpochConfig};
use lb_testing_framework::{DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig};
use lb_utils::math::NonNegativeRatio;
use lb_zone_sdk::Slot;
use logos_blockchain_tests::{
    common::manual_cluster::{
        ManualNodeLayout, start_local_manual_cluster_with_layout, wait_for_nodes_tip_slot,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::DynError;
use tokio::time::sleep;

const NODE_COUNT: usize = 2;

/// End-to-end test for blend SDP activity proofs:
///
/// 1. Spawn two validators with blend declarations in the genesis transaction.
/// 2. Wait long enough that declarations would be removed if no activity
///    message was submitted during `inactivity_period + retention_period`
///    epochs.
/// 3. Verify that both declarations are still present, proving that the nodes
///    automatically submitted valid activity messages that the ledger accepted.
#[tokio::test]
async fn sdp_blend_activity() {
    let slots_per_epoch = Arc::new(AtomicU64::new(0));
    let (_base, nodes) = start_local_manual_cluster_with_layout(
        "sdp-blend-activity",
        "mantle-sdp",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_test_context(Some("sdp_blend_activity".to_owned())),
        ),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        {
            let slots_per_epoch = Arc::clone(&slots_per_epoch);
            move |config| Ok::<_, DynError>(test_config(config, &slots_per_epoch))
        },
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await;
    let slots_per_epoch = slots_per_epoch.load(Ordering::Relaxed);

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
    let initial_active_epoch = declarations.values().next().unwrap().active;
    let survival_epochs = initial_active_epoch
        .strict_add(INACTIVITY_PERIOD)
        .strict_add(RETENTION_PERIOD)
        .strict_add(Epoch::new(2)); // +1 margin
    let survival_slots = Slot::new(u64::from(u32::from(survival_epochs)) * slots_per_epoch);
    wait_for_nodes_tip_slot(
        &[&node0.client, &node1.client],
        survival_slots,
        Duration::from_secs(500),
    )
    .await;

    // Declarations must still be present — this proves that activity messages were
    // submitted/accepted, keeping the declarations alive.
    let declarations_after = wait_for_declarations(&node0.client, Duration::from_secs(30)).await;

    // Check if at least one declaration is still present because blocks may have
    // been produced by only one nodes by coincidence
    assert!(
        !declarations_after.is_empty(),
        "At least one blend declaration should survive past the inactivity window. Activity proofs may not have been submitted/accepted"
    );

    // Check that the declarations have the refreshed `active` epoch number.
    for (provider_id, declaration) in declarations {
        let old_active = declaration.active;
        let new_active = declarations_after.get(&provider_id).unwrap().active;
        assert!(
            new_active > old_active,
            "Declaration must have the refreshed `active` epoch number larger than the initial one ({old_active:?}), but got {new_active:?}"
        );
    }
}

const INACTIVITY_PERIOD: NumberOfEpochs = NumberOfEpochs::new(2);
const RETENTION_PERIOD: NumberOfEpochs = NumberOfEpochs::new(1);

fn test_config(mut config: RunConfig, slots_per_epoch: &AtomicU64) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);

    // Set the epoch length not too long to speed up the test,
    // but also not too short because we want blend nodes to collect blend tokens
    // every epoch to keep their declarations alive.
    config.deployment.cryptarchia.epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    config.deployment.cryptarchia.security_param = NonZero::new(4).unwrap();
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());

    slots_per_epoch.store(
        config.deployment.cryptarchia.slots_per_epoch(),
        Ordering::Relaxed,
    );

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

    // Shorten Blend delay to speed up the test
    config
        .deployment
        .blend
        .core
        .scheduler
        .delayer
        .maximum_release_delay_in_rounds = 1.try_into().unwrap();
    // Set num_blend_layers to NODE_COUNT (instead of 1) to increase
    // the probability that all nodes can collect a blend token from
    // a single blend message.
    config.deployment.blend.common.num_blend_layers = (NODE_COUNT as u64).try_into().unwrap();

    config
}

async fn wait_for_declarations(
    node: &NodeHttpClient,
    duration: Duration,
) -> HashMap<ProviderId, Declaration> {
    tokio::time::timeout(duration, async {
        loop {
            if let Ok(declarations) = node.get_sdp_declarations().await {
                return declarations
                    .into_iter()
                    .map(|declaration| (declaration.provider_id, declaration))
                    .collect();
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("SDP declarations should become available")
}
