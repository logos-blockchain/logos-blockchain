use std::time::Duration;

use futures::StreamExt as _;
use lb_libp2p::protocol_name::StreamProtocol;
use lb_node::config::{DeploymentSettings, deployment::devnet};
use logos_blockchain_tests::{
    nodes::{Validator, create_validator_config},
    topology::configs::create_general_configs,
};
use time::OffsetDateTime;

// To run use:
// ```bash
// cargo test -p logos-blockchain-tests blend_debug_setup -- --nocapture --ignored
// ```
#[ignore = "For local debugging"]
#[tokio::test]
async fn blend_devnet_setup() {
    let (configs, genesis_tx) = create_general_configs(4, Some("blend_devnet_setup"));

    let deployment_settings = {
        let mut devnet_settings =
            serde_yaml::from_str::<DeploymentSettings>(devnet::SERIALIZED_DEPLOYMENT).unwrap();
        devnet_settings.cryptarchia.genesis_state = genesis_tx;
        devnet_settings.time.chain_start_time = OffsetDateTime::now_utc();

        devnet_settings.blend.common.protocol_name =
            StreamProtocol::new("/blend-devnet-setup/blend");
        devnet_settings.network.chain_sync_protocol_name =
            StreamProtocol::new("/blend-devnet-setup/chain_sync");
        devnet_settings.network.kademlia_protocol_name =
            StreamProtocol::new("/blend-devnet-setup/kademlia");
        devnet_settings.network.identify_protocol_name =
            StreamProtocol::new("/blend-devnet-setup/identify");
        devnet_settings.cryptarchia.gossipsub_protocol = "blend-devnet-setup/gossipsub".to_owned();
        devnet_settings.mempool.pubsub_topic = "blend-devnet-setup/mempool".to_owned();

        devnet_settings
    };

    let configs = configs
        .into_iter()
        .map(|c| {
            let mut config = create_validator_config(c, deployment_settings.clone());
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;

            config
        })
        .collect::<Vec<_>>();

    let nodes = futures_util::future::join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let node1 = &nodes[0];
    let stream1 = node1.get_lib_stream().await.unwrap();
    let node2 = &nodes[1];
    let stream2 = node2.get_lib_stream().await.unwrap();
    let node3 = &nodes[2];
    let stream3 = node3.get_lib_stream().await.unwrap();
    let node4 = &nodes[3];
    let stream4 = node4.get_lib_stream().await.unwrap();

    tokio::pin!(stream1);
    tokio::pin!(stream2);
    tokio::pin!(stream3);
    tokio::pin!(stream4);

    loop {
        tokio::select! {
            Some(lib1) = stream1.next() => {
                println!("--------------------------------------------------");
                println!("--------------------------------------------------");
                println!("Node 1 LIB: height={}", lib1.height);
            }
            Some(lib2) = stream2.next() => {
                println!("--------------------------------------------------");
                println!("--------------------------------------------------");
                println!("Node 2 LIB: height={}", lib2.height);
            }
            Some(lib3) = stream3.next() => {
                println!("--------------------------------------------------");
                println!("--------------------------------------------------");
                println!("Node 3 LIB: height={}", lib3.height);
            }
            Some(lib4) = stream4.next() => {
                println!("--------------------------------------------------");
                println!("--------------------------------------------------");
                println!("Node 4 LIB: height={}", lib4.height);
            }
        }
        println!("--------------------------------------------------");
        println!("{:?}", node1.consensus_info(false).await);
        println!("{:?}", node2.consensus_info(false).await);
        println!("{:?}", node3.consensus_info(false).await);
        println!("{:?}", node4.consensus_info(false).await);
        println!("--------------------------------------------------");
        println!("--------------------------------------------------");
    }
}
