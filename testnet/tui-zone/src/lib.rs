mod message;
mod state;
mod ui;

use std::{fs, path::Path};

use clap::Parser;
use lb_core::mantle::ops::channel::ChannelId;
use lb_key_management_system_service::keys::{ED25519_SECRET_KEY_SIZE, Ed25519Key};
use lb_zone_sdk::{
    CommonHttpClient,
    adapter::NodeHttpClient,
    sequencer::{Event, ZoneSequencer},
};
use reqwest::Url;
use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::{
    message::AppMessage,
    state::{InMemoryZoneState, ZoneState as _, resolve_conflicts},
};

#[derive(Parser, Debug)]
#[command(about = "Terminal UI zone sequencer - publish text inscriptions")]
pub struct InscribeArgs {
    /// Logos blockchain node HTTP endpoint
    #[arg(long, default_value = "http://localhost:8080", env = "NODE_URL")]
    node_url: String,

    /// Path to the signing key file (created if it doesn't exist)
    #[arg(long, default_value = "sequencer.key", env = "KEY_PATH")]
    key_path: String,
}

pub async fn run(args: InscribeArgs) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let node_url: Url = args.node_url.parse().expect("invalid node URL");
    let signing_key = load_or_create_signing_key(Path::new(&args.key_path));
    let channel_id = ChannelId::from(signing_key.public_key().to_bytes());

    println!("TUI Zone Sequencer");
    println!("  Node:       {node_url}");
    println!("  Key:        {}", args.key_path);
    println!("  Channel ID: {}", hex::encode(channel_id.as_ref()));
    println!();

    let mut state = InMemoryZoneState::default();
    let checkpoint = state.load_checkpoint().cloned();

    let node = NodeHttpClient::new(CommonHttpClient::new(None), node_url);
    let (mut sequencer, handle) = ZoneSequencer::init(channel_id, signing_key, node, checkpoint);

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let mut stdin_rx = spawn_stdin_reader(ready_rx);
    let mut ready_tx = Some(ready_tx);

    println!("Bootstrapping sequencer...");

    loop {
        tokio::select! {
            event = sequencer.next_event() => {
                let Some(event) = event else {
                    debug!("next_event returned None, retrying...");
                    continue;
                };

                match event {
                    Event::Ready => {
                        if let Some(tx) = ready_tx.take() {
                            let _ = tx.send(());
                        }
                        println!("Ready.");
                        println!();
                        println!("Type a message and press Enter to publish.");
                        println!("Press Ctrl-D or type an empty line to exit.");
                        println!();
                        ui::render_state(&state);
                        ui::prompt();
                    }
                    Event::ChannelUpdate {
                        invalidated,
                        adopted,
                        ..
                    } => {
                        if invalidated.is_empty() && adopted.is_empty() {
                            continue;
                        }

                        let to_republish =
                            resolve_conflicts(&mut state, &invalidated, &adopted);

                        for msg in to_republish {
                            if let Err(e) = handle.publish_message(msg.to_bytes()).await {
                                error!("failed to re-publish: {e}");
                                break;
                            }
                        }

                        ui::render_state(&state);
                        ui::prompt();
                    }
                    Event::TxsFinalized { inscriptions, .. } => {
                        let payloads: Vec<Vec<u8>> =
                            inscriptions.iter().map(|i| i.payload.clone()).collect();
                        state.finalize(&payloads);
                        ui::render_state(&state);
                        ui::prompt();
                    }
                    Event::Published { checkpoint, .. } => {
                        state.save_checkpoint(checkpoint);
                    }
                    Event::FinalizedInscriptions { .. } => {}
                }
            }

            input = stdin_rx.recv() => {
                let Some(text) = input else {
                    println!();
                    break;
                };

                let msg = AppMessage::new(text);
                debug!("publishing \"{}\" id={}", msg.text, msg.tx_id);
                if let Err(e) = handle.publish_message(msg.to_bytes()).await {
                    error!("failed to publish: {e}");
                    break;
                }
                ui::prompt();
            }

            _ = tokio::signal::ctrl_c() => {
                println!();
                break;
            }
        }
    }

    println!("Goodbye!");
}

fn load_or_create_signing_key(path: &Path) -> Ed25519Key {
    if path.exists() {
        let key_bytes = fs::read(path).expect("failed to read key file");
        assert!(
            key_bytes.len() == ED25519_SECRET_KEY_SIZE,
            "invalid key file: expected {} bytes, got {}",
            ED25519_SECRET_KEY_SIZE,
            key_bytes.len()
        );
        let key_array: [u8; ED25519_SECRET_KEY_SIZE] =
            key_bytes.try_into().expect("length already checked");
        Ed25519Key::from_bytes(&key_array)
    } else {
        let mut key_bytes = [0u8; ED25519_SECRET_KEY_SIZE];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key_bytes);
        fs::write(path, key_bytes).expect("failed to write key file");
        Ed25519Key::from_bytes(&key_bytes)
    }
}

fn spawn_stdin_reader(ready: tokio::sync::oneshot::Receiver<()>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(16);
    std::thread::spawn(move || {
        // Wait until the sequencer is ready before accepting input
        if ready.blocking_recv().is_err() {
            return;
        }

        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = line.trim_end().to_owned();
                    if text.is_empty() || tx.blocking_send(text).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}
