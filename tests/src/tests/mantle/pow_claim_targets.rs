use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use lb_groth16::Fr;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_node::config::{RunConfig, pow::serde::ClaimTarget};
use lb_testing_framework::{
    DeploymentBuilder, LbcManualCluster, TopologyConfig as TfTopologyConfig,
};
use logos_blockchain_tests::{
    common::manual_cluster::build_local_manual_cluster, cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::{DynError, PeerSelection, StartNodeOptions};

/// How long a node has to become ready. The node paying a key of its wallet
/// shows it is enough.
const START_TIMEOUT: Duration = Duration::from_mins(1);

/// A `PoW` auto-claim target must be a wallet key of the config: a node paying
/// a key of its wallet starts, and the same node paying a key its wallet does
/// not track refuses to start.
#[tokio::test]
#[expect(
    clippy::large_futures,
    reason = "Manual-cluster start operations return large futures in tests; boxing adds noise without improving the test"
)]
async fn a_node_refuses_to_start_with_an_untracked_claim_target() {
    let base = build_local_manual_cluster(
        "pow-claim-targets",
        "mantle-pow",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(2).with_test_context(Some(
                "a_node_refuses_to_start_with_an_untracked_claim_target".to_owned(),
            )),
        ),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    );
    let cluster = base.cluster();

    start(cluster, base.scenario_base_dir(), "node-0", None).await;
    cluster
        .wait_node_ready("node-0")
        .await
        .expect("a node paying a key of its wallet should start");

    let untracked = ZkPublicKey::new(Fr::from(1u64));
    start(cluster, base.scenario_base_dir(), "node-1", Some(untracked)).await;
    assert!(
        cluster.wait_node_ready("node-1").await.is_err(),
        "a node paying a key its wallet does not track should not start"
    );
}

/// Starts the node `name` with auto-claim paying `target`, or the leader
/// funding key, a key of its wallet, when it is `None`. A node that refuses to
/// start may already fail here, before its readiness is awaited.
#[expect(
    clippy::large_futures,
    reason = "Manual-cluster start operations return large futures in tests; boxing adds noise without improving the test"
)]
async fn start(
    cluster: &LbcManualCluster,
    scenario_base_dir: &Path,
    name: &str,
    target: Option<ZkPublicKey>,
) {
    drop(
        cluster
            .start_node_with(
                name,
                StartNodeOptions::default()
                    .with_peers(PeerSelection::None)
                    .with_persist_dir(scenario_base_dir.join(name))
                    .with_start_timeout(START_TIMEOUT)
                    .create_patch(move |config| Ok::<_, DynError>(paying(config, target))),
            )
            .await,
    );
}

fn paying(mut config: RunConfig, target: Option<ZkPublicKey>) -> RunConfig {
    let public_key = target.unwrap_or(config.user.cryptarchia.leader.wallet.funding_pk);
    config.user.pow.auto_claim.targets = vec![ClaimTarget {
        public_key,
        threshold: 1,
    }];
    config
}
