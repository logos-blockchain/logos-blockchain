use std::{num::NonZero, time::Duration};

use futures_util::StreamExt as _;
use logos_blockchain_tests::{
    adjust_timeout,
    common::time::max_block_propagation_time,
    nodes::validator::{Validator, create_validator_config},
    topology::configs::create_general_configs,
};
use serial_test::serial;

const TARGET_IMMUTABLE_BLOCK_COUNT: u32 = 5;

#[tokio::test]
#[serial]
async fn immutable_blocks_two_nodes() {
    let configs = create_general_configs(2)
        .into_iter()
        .map(|c| {
            let mut config = create_validator_config(c);
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

    let deployment = &configs[0].deployment;
    let blocks_to_wait = deployment.cryptarchia.security_param.get() + TARGET_IMMUTABLE_BLOCK_COUNT;
    let timeout = max_block_propagation_time(
        blocks_to_wait,
        configs.len().try_into().unwrap(),
        deployment,
        2.0,
    );

    let nodes = futures_util::future::join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let [node1, node2] = &nodes[..] else {
        panic!("Incorrect number of validators");
    };

    let (stream1, stream2) = (
        node1.get_lib_stream().await.unwrap(),
        node2.get_lib_stream().await.unwrap(),
    );

    tokio::pin!(stream1);
    tokio::pin!(stream2);

    let timeout = tokio::time::sleep(adjust_timeout(timeout));

    tokio::select! {
        () = timeout => panic!("Timed out waiting for matching LIBs"),
        () = async {
            let mut stream = stream1.zip(stream2);

            while let Some((lib1, lib2)) = stream.next().await {
                println!("Node 1 LIB: height={}, id={}", lib1.height, lib1.header_id);
                println!("Node 2 LIB: height={}, id={}", lib2.height, lib2.header_id);

                assert!(!(lib1 != lib2),
                    "LIBs mismatched! Node 1: {lib1:?}, Node 2: {lib2:?}");

                if lib1.height >= u64::from(TARGET_IMMUTABLE_BLOCK_COUNT) { return; }
            }

            panic!("LIB stream failed");
        } => {}
    }
}
