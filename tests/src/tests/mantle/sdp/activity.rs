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
use lb_core::{
    mantle::Value,
    sdp::{Declaration, NumberOfEpochs, ProviderId, ServiceType},
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_node::config::{RunConfig, cryptarchia::deployment::EpochConfig};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, NodeHttpClient, TopologyConfig as TfTopologyConfig,
};
use lb_utils::math::NonNegativeRatio;
use lb_zone_sdk::Slot;
use logos_blockchain_tests::{
    common::manual_cluster::{
        ManualNodeLayout, start_local_manual_cluster_with_layout, wait_for_nodes_tip_slot,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::{DynError, StartedNode};
use tokio::time::sleep;

const NODE_COUNT: usize = 2;

/// End-to-end test for blend SDP activity proofs:
///
/// 1. Spawn two validators with blend declarations in the genesis transaction.
/// 2. Wait past `inactivity_period` so that any activity messages produced by
///    the nodes have to refresh the `active` field on the declarations.
/// 3. Verify that each declaration's `active` epoch has advanced past its
///    initial value, proving that the nodes automatically submitted valid
///    activity messages that the ledger accepted.
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

    // Snapshot each provider's wallet initial balance.
    let provider_zk_ids: Vec<ZkPublicKey> = declarations.values().map(|d| d.zk_id).collect();
    let initial_balances = collect_provider_balances(&nodes, &provider_zk_ids).await;

    // Wait past `inactivity_period` so any activity messages produced by the
    // nodes have to refresh the `active` field on the declarations.
    let initial_active_epoch = declarations.values().next().unwrap().active;
    let target_epoch = initial_active_epoch
        .strict_add(INACTIVITY_PERIOD)
        .strict_add(Epoch::new(2)); // +1 margin
    let target_slot = Slot::new(u64::from(u32::from(target_epoch)) * slots_per_epoch);
    wait_for_nodes_tip_slot(
        &[&node0.client, &node1.client],
        target_slot,
        Duration::from_secs(500),
    )
    .await;

    // Each declaration's `active` epoch must have advanced past its initial
    // value, proving activity messages were submitted and accepted.
    let declarations_after = wait_for_declarations(&node0.client, Duration::from_secs(30)).await;

    // Check if at least one declaration is still present because blocks may have
    // been produced by only one nodes by coincidence
    assert!(
        !declarations_after.is_empty(),
        "At least one blend declaration should survive past the inactivity window. Activity proofs may not have been submitted/accepted"
    );

    // Check that the survived declarations have the refreshed `active` epoch.
    for (provider_id, declaration) in declarations_after {
        let old_active = declarations.get(&provider_id).unwrap().active;
        let new_active = declaration.active;
        assert!(
            new_active > old_active,
            "Declaration must have the refreshed `active` epoch number larger than the initial one ({old_active:?}), but got {new_active:?}"
        );
    }

    // At least one provider's wallet balance must have grown.
    let final_balances = collect_provider_balances(&nodes, &provider_zk_ids).await;
    let anyone_paid = provider_zk_ids.iter().any(|zk_id| {
        let before = initial_balances.get(zk_id).copied().unwrap_or(0);
        let after = final_balances.get(zk_id).copied().unwrap_or(0);
        after > before
    });
    assert!(
        anyone_paid,
        "expected at least one provider's wallet balance to grow; no reward UTXOs were credited",
    );
}

const INACTIVITY_PERIOD: NumberOfEpochs = NumberOfEpochs::new(2);

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

    // Set a small inactivity period so the inactivity window is short enough
    // for the test to observe `active` being refreshed quickly.
    let blend_params = config
        .deployment
        .cryptarchia
        .sdp_config
        .service_params
        .get_mut(&ServiceType::BlendNetwork)
        .expect("blend network params should exist");
    blend_params.inactivity_period = INACTIVITY_PERIOD.try_into().unwrap();

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

/// For each `zk_id`, ask every node for the wallet balance.
async fn collect_provider_balances(
    nodes: &[StartedNode<LbcEnv>],
    zk_ids: &[ZkPublicKey],
) -> HashMap<ZkPublicKey, Value> {
    let mut balances = HashMap::new();
    for &zk_id in zk_ids {
        for node in nodes {
            if let Ok(response) = node.client.wallet_balance(zk_id, None).await {
                balances.insert(zk_id, response.balance);
                break;
            }
        }
    }
    balances
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
