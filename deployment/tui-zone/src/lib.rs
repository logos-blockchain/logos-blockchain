mod message;
mod state;
mod ui;

use std::{fs, path::Path};

use clap::Parser;
use lb_core::mantle::ops::channel::{ChannelId, inscribe::Inscription};
use lb_key_management_system_service::keys::{ED25519_SECRET_KEY_SIZE, Ed25519Key};
use lb_zone_sdk::{
    CommonHttpClient,
    adapter::NodeHttpClient,
    sequencer::{Event, FinalizedOp, FinalizedTx, InscriptionInfo, ZoneSequencer},
};
use reqwest::Url;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::{
    message::AppMessage,
    state::{InMemoryZoneState, ZoneState as _},
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

// Load signing key from file or generate a new one if it doesn't exist
//
// path: The path to the signing key file
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

// Defines initial post-bootstrapping behaviour
//
// state: Zone state
// ready_tx: The transmitter of the ready event to ready_rx
fn handle_ready(
    state: &InMemoryZoneState,
    ready_tx: &mut Option<tokio::sync::oneshot::Sender<()>>,
) {
    info!("Sequencer ready");
    if let Some(tx) = ready_tx.take() {
        let _ = tx.send(());
    }
    println!("Ready.");
    println!();
    println!("Type a message and press Enter to publish.");
    println!("Press Ctrl-D or type an empty line to exit.");
    println!();
    ui::render_state(state);
    ui::prompt();
}

// Apply finalised messages from chain to Zone state
//
// items: Finalised transactions
// state: Zone state
fn apply_finalized(items: &[FinalizedTx], state: &mut InMemoryZoneState) {
    // TUI only cares about inscriptions for rendering; deposit / withdraw
    // ops have no inscription payload.
    let inscriptions: Vec<InscriptionInfo> = items
        .iter()
        .flat_map(|t| t.ops.iter())
        .filter_map(|op| match op {
            FinalizedOp::Inscription(i) => Some(i.clone()),
            FinalizedOp::Deposit(_) | FinalizedOp::Withdraw(_) => None,
        })
        .collect();
    state.on_finalized(&inscriptions);
    ui::render_state(state);
    ui::prompt();
}

// Handle sequencer events
//
// event: current sequencer event
fn handle_event(
    event: Event,
    state: &mut InMemoryZoneState,
    ready_tx: &mut Option<tokio::sync::oneshot::Sender<()>>,
) {
    match event {
        Event::Ready => handle_ready(state, ready_tx),

        // Centralized Zone: ignore `channel_update` — a single sequencer
        // never loses a slot, so there is nothing to adopt or roll back.
        // Just apply finalised inscriptions and persist the checkpoint.
        Event::BlocksProcessed {
            checkpoint,
            finalized,
            ..
        } => {
            if !finalized.is_empty() {
                apply_finalized(&finalized, state);
            }
            state.save_checkpoint(checkpoint);
        }
        Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
    }
}

// Processing loop
//
// args: Setup info
pub async fn run(args: InscribeArgs) {
    // Get node URL
    let node_url: Url = args.node_url.parse().expect("invalid node URL");

    // Create new signing key or load existing one from path
    let signing_key = load_or_create_signing_key(Path::new(&args.key_path));

    // Derive channel ID
    let channel_id = ChannelId::from(signing_key.public_key().to_bytes());

    println!("TUI Zone Sequencer");
    println!("  Node:       {node_url}");
    println!("  Key:        {}", args.key_path);
    println!("  Channel ID: {}", hex::encode(channel_id.as_ref()));
    println!();

    // Create initial Zone state & load checkpoint (to be implemented later in the
    // tutorial)
    let mut state = InMemoryZoneState::default();
    let checkpoint = state.load_checkpoint().cloned();

    // Connect to node
    let node = NodeHttpClient::new(CommonHttpClient::new(None), node_url);

    // Initialise ZoneSequencer
    let mut sequencer = ZoneSequencer::init(channel_id, signing_key, node, checkpoint);

    // Wait to start reading from stdin until ready
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let mut stdin_rx = spawn_stdin_reader(ready_rx);
    let mut ready_tx = Some(ready_tx);

    loop {
        tokio::select! {

            // Watch for events
            event = sequencer.next_event() => {
                handle_event(event, &mut state, &mut ready_tx);
            }

            // Get input text from stdin
            input = stdin_rx.recv() => {

                // Handle unexpected input
                let Some(text) = input else {
                    println!();
                    break;
                };

                // Turn input into AppMessage wrapper. Includes unique ID for each transaction to avoid removing 'duplicate' messages
                // from mempool
                let msg = AppMessage::new(text);
                debug!(tx_uuid = %msg.tx_uuid, text = %msg.text, "Publishing message");

                // publish() takes bytes anyway, so must convert
                let Ok(inscription) = Inscription::try_from(msg.to_bytes()) else {
                    error!("Message is too large to fit in an inscription");
                    continue;
                };

                // Publish data
                match sequencer.handle().publish(inscription) {

                    // Get result and checkpoint, update Zone state
                    Ok((result, checkpoint)) => {
                        let info = result.tx.inscription();
                        debug!(msg_id = %hex::encode(info.this_msg.as_ref()), "Published");
                        state.on_published(info);
                        state.save_checkpoint(checkpoint);
                        ui::render_state(&state);
                        eprintln!("  \x1b[90mpending...\x1b[0m");
                        ui::prompt();
                    }
                    Err(lb_zone_sdk::sequencer::Error::Unavailable { reason }) => {
                        warn!("publish rejected: {reason}");
                        eprintln!(
                            "  \x1b[33msequencer is still starting up, try again in a moment\x1b[0m"
                        );
                        ui::prompt();
                    }
                    Err(e) => {
                        error!("failed to publish: {e}");
                        break;
                    }
                }
            }

            // Ctrl+C exits the loop
            _ = tokio::signal::ctrl_c() => {
                println!();
                break;
            }
        }
    }

    println!("Goodbye!");
}
