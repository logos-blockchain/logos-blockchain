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
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use lb_zone_sdk::Slot;
use logos_blockchain_tests::{
    common::manual_cluster::{
        LocalManualClusterHarnessBase, build_local_manual_cluster, wait_for_nodes_tip_slot,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::{DynError, PeerSelection, StartNodeOptions, StartedNode};
use tokio::time::sleep;

const NODE_COUNT: usize = 2;
/// Number of genesis notes given to each node's SDP funding wallet, so that a
/// note encumbered by a rejected activity tx doesn't block resubmissions.
const SDP_FUNDING_NOTES: usize = 5;

/// End-to-end test for blend SDP activity proofs:
///
/// 1. Spawn two validators with blend declarations in the genesis transaction,
///    each with a dedicated SDP funding wallet holding multiple notes.
/// 2. Wait `inactivity_period * 3` epochs so that any activity messages
///    produced by the nodes have to refresh the `active` field on the
///    declarations repeatedly.
/// 3. Verify that each declaration's `active` epoch has advanced past its
///    initial value, proving that the nodes automatically submitted valid
///    activity messages that the ledger accepted.
#[tokio::test]
async fn sdp_blend_activity() {
    let slots_per_epoch = Arc::new(AtomicU64::new(0));
    let (_base, nodes) = start_nodes_with_sdp_wallets(&slots_per_epoch).await;
    let slots_per_epoch = slots_per_epoch.load(Ordering::Relaxed);

    let node0 = &nodes[0];

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

    // Run well beyond `inactivity_period` so that the nodes have to refresh
    // the `active` field on the declarations repeatedly.
    let test_epochs = NumberOfEpochs::new(INACTIVITY_PERIOD.into_inner() * 3);
    let target_epoch = declarations
        .values()
        .next()
        .unwrap()
        .active
        .strict_add(test_epochs);
    let storage_prices = wait_for_epoch(&nodes, target_epoch, slots_per_epoch).await;
    // The activity txs are storage usage, so the storage gas price is expected
    // to rise, which makes activity txs funded against the previous epoch's
    // price rejected and resubmitted. Printed for reference only.
    println!("storage gas prices per epoch: {storage_prices:?}");

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

/// Starts `NODE_COUNT` nodes, each with a dedicated SDP funding wallet holding
/// `SDP_FUNDING_NOTES` genesis notes.
async fn start_nodes_with_sdp_wallets(
    slots_per_epoch: &Arc<AtomicU64>,
) -> (LocalManualClusterHarnessBase, Vec<StartedNode<LbcEnv>>) {
    let sdp_wallets: Vec<WalletAccount> = (0..NODE_COUNT as u64)
        .map(|n| WalletAccount::deterministic(n, 1_000_000, false).unwrap())
        .collect();
    let mut accounts = Vec::with_capacity(NODE_COUNT * SDP_FUNDING_NOTES);
    for sdp_wallet in &sdp_wallets {
        accounts.extend(std::iter::repeat_n(sdp_wallet.clone(), SDP_FUNDING_NOTES));
    }

    let base = build_local_manual_cluster(
        "sdp-blend-activity",
        "mantle-sdp",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some("sdp_blend_activity".to_owned())),
        )
        .with_wallet_config(WalletConfig { accounts }),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    );

    let mut nodes: Vec<StartedNode<LbcEnv>> = Vec::with_capacity(NODE_COUNT);
    for (index, sdp_wallet) in sdp_wallets.iter().enumerate() {
        let peers = nodes.first().map_or(PeerSelection::None, |seed| {
            PeerSelection::Named(vec![seed.name.clone()])
        });
        let sdp_funding_pk = sdp_wallet.public_key();
        let slots_per_epoch = Arc::clone(slots_per_epoch);
        let node = Box::pin(
            base.cluster().start_node_with(
                &index.to_string(),
                StartNodeOptions::default()
                    .with_peers(peers)
                    .with_persist_dir(base.scenario_base_dir().join(format!("node-{index}")))
                    .create_patch(move |mut config: RunConfig| {
                        config.user.sdp.wallet.funding_pk = sdp_funding_pk;
                        Ok::<_, DynError>(test_config(config, &slots_per_epoch))
                    }),
            ),
        )
        .await
        .unwrap_or_else(|error| panic!("starting node-{index} should succeed: {error}"));
        nodes.push(node);
    }
    base.cluster()
        .wait_network_ready()
        .await
        .expect("manual cluster should become ready");

    (base, nodes)
}

/// Waits until all nodes reach `target_epoch`, recording the storage gas
/// price of the first node at every epoch boundary.
async fn wait_for_epoch(
    nodes: &[StartedNode<LbcEnv>],
    target_epoch: Epoch,
    slots_per_epoch: u64,
) -> Vec<Value> {
    let clients: Vec<&NodeHttpClient> = nodes.iter().map(|node| &node.client).collect();
    let mut storage_prices = Vec::new();
    for epoch in 0..=u32::from(target_epoch) {
        wait_for_nodes_tip_slot(
            &clients,
            Slot::new(u64::from(epoch) * slots_per_epoch),
            Duration::from_secs(120),
        )
        .await;
        let gas_prices = clients[0]
            .gas_prices(None)
            .await
            .expect("gas prices should be available");
        storage_prices.push(gas_prices.storage_gas_price.into_inner());
    }
    storage_prices
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
                    .into_values()
                    .map(|declaration| (declaration.provider_id, declaration))
                    .collect();
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("SDP declarations should become available")
}
