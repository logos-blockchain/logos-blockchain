#![expect(clippy::non_ascii_literal, reason = "Demo, so emojis are fine.")]

use core::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
};
use std::sync::Arc;

use axum::{
    Router,
    response::sse::{Event, Sse},
    routing::get,
};
use broadcast_service::BlockInfo;
use clap::Parser;
use common_http_client::{BasicAuthCredentials, CommonHttpClient};
use demo_sequencer::BlockData;
use futures::{Stream, StreamExt as _};
use nomos_core::{
    block::Block,
    mantle::{
        Op, SignedMantleTx,
        ops::channel::{ChannelId, inscribe::InscriptionOp},
    },
};
use owo_colors::OwoColorize as _;
use tokio::{net::TcpListener, select, signal::ctrl_c, sync::broadcast};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use url::Url;

const BANNER: &str = r"
    _   __                           ___              __    _                
   / | / /___  ____ ___  ____  _____/   |  __________/ /_  (_)   _____  _____
  /  |/ / __ \/ __ `__ \/ __ \/ ___/ /| | / ___/ ___/ __ \/ / | / / _ \/ ___/
 / /|  / /_/ / / / / / / /_/ (__  ) ___ |/ /  / /__/ / / / /| |/ /  __/ /    
/_/ |_/\____/_/ /_/ /_/\____/____/_/  |_/_/   \___/_/ /_/_/ |___/\___/_/     
";

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
    #[clap(short = 't', env = "TOKEN_NAME")]
    token_name: String,
}

fn print_startup_banner(endpoint: &Url, channel_id: &str, listen_addr: &SocketAddr) {
    println!("{}", BANNER.cyan().bold());
    println!("{}", "═".repeat(70).dimmed());
    println!(
        "  {} {}",
        "📡 Nomos Node:".bright_blue().bold(),
        endpoint.to_string().white()
    );
    println!(
        "  {} {}",
        "📺 Channel ID:".bright_blue().bold(),
        channel_id.white()
    );
    println!(
        "  {} {}",
        "🌐 HTTP Server:".bright_blue().bold(),
        format!("http://{listen_addr}/blocks").green()
    );
    println!("{}", "═".repeat(70).dimmed());
    println!("  {} Waiting for blocks...\n", "⏳".yellow());
}

fn process_block(
    block: Block<SignedMantleTx>,
    decoded_channel_id: &ChannelId,
    tx: &broadcast::Sender<BlockData>,
    token_name: &str,
) {
    let block_channel_ops: Vec<BlockData> = block
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
        .collect();

    if block_channel_ops.is_empty() {
        println!("  {} No inscriptions in this block", "○".dimmed());
    } else {
        for block_data in &block_channel_ops {
            println!("{}", "┌".to_owned().bright_green());
            println!(
                "│ {} Block #{}",
                "📦".green(),
                block_data.block_id.to_string().bright_green().bold()
            );
            println!(
                "│ 💳 {} transaction(s)",
                block_data.transactions.len().to_string().yellow().bold()
            );

            for tx_item in &block_data.transactions {
                println!(
                    "│   {} {} → {} ({} {})",
                    "↳".dimmed(),
                    tx_item.from.bright_cyan(),
                    tx_item.to.bright_magenta(),
                    tx_item.amount.to_string().yellow(),
                    token_name
                );
            }
            println!("{}", "└".to_owned().bright_green());
        }

        // Broadcast each block to connected SSE clients
        for block_data in block_channel_ops {
            drop(tx.send(block_data));
        }
    }
}

fn blocks_stream(
    broadcast_rx: broadcast::Receiver<BlockData>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(broadcast_rx).filter_map(async |result| match result {
        Ok(block_data) => {
            let json = serde_json::to_string(&block_data).ok()?;
            Some(Ok(Event::default().data(json)))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

#[derive(Clone)]
struct AppState {
    block_tx: broadcast::Sender<BlockData>,
}

async fn handle_blocks_stream(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    blocks_stream(state.block_tx.subscribe())
}

#[tokio::main]
async fn main() {
    let CliArgs {
        nomos_node_http_endpoint,
        username,
        password,
        channel_id,
        token_name,
    } = CliArgs::parse();

    let decoded_channel_id: ChannelId = <[u8; 32]>::try_from(hex::decode(&channel_id).unwrap())
        .unwrap()
        .into();
    let listen_address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080));

    print_startup_banner(&nomos_node_http_endpoint, &channel_id, &listen_address);

    let client = Arc::new(CommonHttpClient::new(Some(BasicAuthCredentials::new(
        username,
        Some(password),
    ))));

    // Create broadcast channel for SSE
    let (block_tx, _) = broadcast::channel::<BlockData>(100);
    let block_tx_clone = block_tx.clone();

    // Set up cancellation token
    let cancel_token = CancellationToken::new();
    let cancel_token_clone = cancel_token.clone();

    // Spawn a task to listen for Ctrl-C
    tokio::spawn(async move {
        ctrl_c().await.expect("Failed to listen for Ctrl-C");
        println!("\n  {} Graceful shutdown initiated...", "🛑".red());
        cancel_token_clone.cancel();
    });

    // Create the HTTP server router
    let app_state = AppState {
        block_tx: block_tx_clone,
    };
    let app = Router::new()
        .route("/blocks", get(handle_blocks_stream))
        .with_state(app_state);

    // Start HTTP server
    let listener = TcpListener::bind(listen_address)
        .await
        .expect("Failed to bind to address");
    let cancel_token_http = cancel_token.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                cancel_token_http.cancelled().await;
            })
            .await
            .expect("HTTP server failed");
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
                println!(
                    "  {} Shutdown complete. Goodbye!",
                    "✅".green()
                );
                break;
            }

            block_info = lib_stream.next() => {
                let Some(BlockInfo { header_id, height }) = block_info else {
                    println!(
                        "  {} Stream ended unexpectedly",
                        "⚠️".yellow()
                    );
                    break;
                };

                println!(
                    "  {} Block at height {} ({})",
                    "🔗".blue(),
                    height.to_string().bright_white().bold(),
                    format!("{}...", &hex::encode(header_id.as_ref()).chars().skip(11).collect::<String>()).dimmed()
                );

                match client.get_block_by_id(nomos_node_http_endpoint.clone(), header_id).await {
                    Ok(Some(block)) => {
                        process_block(block, &decoded_channel_id, &block_tx, token_name.as_str());
                    }
                    Ok(None) => {
                        println!(
                            "  {} Block not found: {:?}",
                            "⚠️".yellow(),
                            header_id
                        );
                    }
                    Err(e) => {
                        println!(
                            "  {} Error fetching block: {}",
                            "❌".red(),
                            e.to_string().red()
                        );
                    }
                }
            }
        }
    }
}
