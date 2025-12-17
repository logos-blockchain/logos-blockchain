use std::sync::Arc;

use broadcast_service::BlockInfo;
use clap::Parser;
use common_http_client::{BasicAuthCredentials, CommonHttpClient};
use demo_sequencer::BlockData;
use futures::StreamExt as _;
use nomos_core::{
    block::Block,
    mantle::{
        Op, SignedMantleTx,
        ops::channel::{ChannelId, inscribe::InscriptionOp},
    },
};
use tokio::{select, signal::ctrl_c};
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Parser, Debug)]
struct CliArgs {
    #[clap(short = 'e', env = "ENDPOINT")]
    nomos_node_http_endpoint: Url,
    #[clap(short = 'u', env = "USERNAME")]
    username: String,
    #[clap(short = 'p', env = "PASSWORD")]
    password: String,
    #[clap(short = 'c', env = "CHANNEL_ID")]
    channel_id: String,
}

fn process_block(block: Block<SignedMantleTx>, decoded_channel_id: &ChannelId) {
    println!(
        "Processing block at height (slot): {:?}",
        block.header().slot()
    );
    let block_channel_ops = block
        .into_transactions()
        .into_iter()
        .flat_map(|tx| tx.mantle_tx.ops)
        .filter_map(|op| match op {
            Op::ChannelInscribe(InscriptionOp {
                channel_id,
                inscription,
                ..
            }) if &channel_id == decoded_channel_id => {
                Some(serde_json::from_slice::<BlockData>(&inscription).unwrap())
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    if block_channel_ops.is_empty() {
        println!("No inscriptions for specified channel in the received block.");
    } else {
        println!(
            "New inscriptions for specified {}: {block_channel_ops:?}",
            hex::encode(decoded_channel_id.as_ref())
        );
    }
}

#[tokio::main]
async fn main() {
    let CliArgs {
        nomos_node_http_endpoint,
        username,
        password,
        channel_id,
    } = CliArgs::parse();

    let decoded_channel_id: ChannelId = <[u8; 32]>::try_from(hex::decode(&channel_id).unwrap())
        .unwrap()
        .into();

    println!("Nomos Node HTTP Endpoint: {nomos_node_http_endpoint}");
    println!("Channel ID: {channel_id:?}");

    let client = Arc::new(CommonHttpClient::new(Some(BasicAuthCredentials::new(
        username,
        Some(password),
    ))));

    // Set up cancellation token
    let cancel_token = CancellationToken::new();
    let cancel_token_clone = cancel_token.clone();

    // Spawn a task to listen for Ctrl-C
    tokio::spawn(async move {
        ctrl_c().await.expect("Failed to listen for Ctrl-C");
        println!("\nReceived Ctrl-C, initiating graceful shutdown...");
        cancel_token_clone.cancel();
    });

    let mut lib_stream = Box::pin(
        client
            .get_lib_stream(nomos_node_http_endpoint.clone())
            .await
            .unwrap(),
    );

    loop {
        select! {
                biased;  // Prioritize cancellation check

                () = cancel_token.cancelled() => {
                    println!("Shutdown complete.");
                    break;
                }

                block_info = lib_stream.next() => {
                    let Some(BlockInfo { header_id, height }) = block_info else {
                        println!("Stream ended.");
                        break;
                    };

                println!("Received block info: height={height}, header_id={header_id:?}");

                match client.get_block_by_id(nomos_node_http_endpoint.clone(), header_id).await {
                    Ok(Some(block)) => {
                        process_block(block, &decoded_channel_id);
                    }
                    Ok(None) => {
                        eprintln!("Block not found: {header_id:?}");
                    }
                    Err(e) => {
                        eprintln!("Error fetching block: {e}");
                    }
                }
            }
        }
    }
}
