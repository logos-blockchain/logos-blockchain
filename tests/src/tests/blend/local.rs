use std::{num::NonZero, time::Duration};

use futures::StreamExt as _;
use logos_blockchain_tests::{
    common::time::max_block_propagation_time,
    nodes::{Validator, create_validator_config},
    topology::configs::{
        create_general_configs, deployment::e2e_deployment_settings_with_genesis_tx,
    },
};

const TARGET_IMMUTABLE_BLOCK_COUNT: u32 = 5;

// To run use:
// ```bash
// cargo test -p logos-blockchain-tests blend_debug_setup -- --nocapture --ignored
// ```
#[ignore = "For local debugging"]
#[tokio::test]
async fn blend_debug_setup() {
    let (configs, genesis_tx) = create_general_configs(4);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
    let configs = configs
        .into_iter()
        .map(|c| {
            let mut config = create_validator_config(c, deployment_settings.clone());
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();

            config
        })
        .collect::<Vec<_>>();

    let blocks_to_wait =
        deployment_settings.cryptarchia.security_param.get() + TARGET_IMMUTABLE_BLOCK_COUNT;
    let timeout = max_block_propagation_time(
        blocks_to_wait,
        configs.len().try_into().unwrap(),
        &deployment_settings,
        2.0,
    );

    for c in &configs {
        println!(
            "Node API available on http://{}/cryptarchia/info",
            c.user.api.backend.listen_address
        );
    }

    let nodes = futures_util::future::join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let node1 = &nodes[0];
    let stream1 = node1.get_lib_stream().await.unwrap();

    tokio::pin!(stream1);

    let timeout = tokio::time::sleep(timeout);

    tokio::select! {
        () = timeout => panic!("Timed out waiting for matching LIBs"),
        () = async {
            while let Some(lib1) = stream1.next().await {
                println!("Node 1 LIB: height={}", lib1.height);

            }
        } => {}
    }
}
