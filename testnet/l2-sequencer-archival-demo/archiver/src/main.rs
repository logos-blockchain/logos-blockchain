#![expect(clippy::non_ascii_literal, reason = "Demo, so emojis are fine.")]

use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use clap::Parser as _;
use common_http_client::{BasicAuthCredentials, CommonHttpClient};
use demo_sequencer::db::AccountDb;
use futures::StreamExt as _;
use owo_colors::OwoColorize as _;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    block::{BlockStream, ValidatedL2Info, validate_block},
    cli::CliArgs,
    ctrl_c::listen_for_sigint,
    db::BlockStore,
    http::Server,
    output::print_startup_banner,
};

mod block;
mod cli;
mod ctrl_c;
mod db;
mod http;
mod output;

#[tokio::main]
async fn main() {
    let CliArgs {
        nomos_node_http_endpoint,
        username,
        password,
        channel_id,
        token_name,
        initial_balance,
        port_number,
        blocks_db_path,
        accounts_db_path,
    } = CliArgs::parse();

    let listen_address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port_number));

    print_startup_banner(&nomos_node_http_endpoint, &channel_id, &listen_address);

    // Setup

    let (rollup_block_sender, _) = broadcast::channel::<ValidatedL2Info>(100);

    let cancellation_token = CancellationToken::new();

    let client = CommonHttpClient::new(username.map(|u| BasicAuthCredentials::new(u, password)));

    let blocks_db = BlockStore::new(&blocks_db_path).unwrap();
    let accounts_db = AccountDb::new(&accounts_db_path, initial_balance).unwrap();

    // Start sigint handler

    listen_for_sigint(cancellation_token.clone());

    // Start HTTP server

    Server::new(
        rollup_block_sender.subscribe(),
        cancellation_token.clone(),
        blocks_db.clone(),
    )
    .start(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8090).into());

    // Start LIB subscriber

    let mut block_stream = Box::pin(BlockStream::create(
        cancellation_token,
        client,
        &nomos_node_http_endpoint,
        &channel_id,
        token_name.as_str(),
    ));

    while let Some(block) = block_stream.next().await {
        match validate_block(block.data, &accounts_db, &blocks_db).await {
            Ok(validated_l2_block) => {
                let validated_l2_info = ValidatedL2Info::new(
                    validated_l2_block.clone(),
                    block.l1_block_id,
                    block.l1_transaction_id,
                );
                blocks_db
                    .add_block(validated_l2_info.clone())
                    .await
                    .unwrap();
                let block_id = validated_l2_block.as_ref().block_id;
                if blocks_db.unmark_block_as_invalid(block_id).await.unwrap() {
                    println!(
                        "  {} Previously invalid block {block_id} now marked as valid",
                        "✅".green(),
                    );
                }
                rollup_block_sender.send(validated_l2_info).unwrap();
            }
            Err(invalid_l2_block) => {
                blocks_db
                    .mark_block_as_invalid(invalid_l2_block.block_id)
                    .await
                    .unwrap();
            }
        }
    }
}
