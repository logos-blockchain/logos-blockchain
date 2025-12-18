#![expect(clippy::non_ascii_literal, reason = "Demo, so emojis are fine.")]

use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use clap::Parser as _;
use common_http_client::{BasicAuthCredentials, CommonHttpClient};
use demo_sequencer::BlockData;
use futures::StreamExt as _;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    block::BlockStream, cli::CliArgs, ctrl_c::listen_for_sigint, http::Server,
    output::print_startup_banner,
};

mod block;
mod cli;
mod ctrl_c;
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
    } = CliArgs::parse();

    let listen_address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8090));

    print_startup_banner(&nomos_node_http_endpoint, &channel_id, &listen_address);

    // Setup

    let (rollup_block_sender, _) = broadcast::channel::<BlockData>(100);

    let cancellation_token = CancellationToken::new();

    let client = CommonHttpClient::new(Some(BasicAuthCredentials::new(username, Some(password))));

    // Start sigint handler

    listen_for_sigint(cancellation_token.clone());

    // Start HTTP server

    Server::new(rollup_block_sender.subscribe(), cancellation_token.clone())
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
        rollup_block_sender.send(block).unwrap();
    }
}
