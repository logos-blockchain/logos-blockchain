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
use tracing::{debug, error, info};

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
                if let Some(event) = event {
                    handle_event(event, &mut state, &handle, &mut ready_tx).await;
                }
            }

            input = stdin_rx.recv() => {
                let Some(text) = input else {
                    println!();
                    break;
                };

                let msg = AppMessage::new(text);
                debug!(tx_uuid = %msg.tx_uuid, text = %msg.text, "Publishing message");
                if let Err(e) = handle.publish_message(msg.to_bytes()).await {
                    error!("failed to publish: {e}");
                    break;
                }
                eprintln!("  \x1b[90mpending...\x1b[0m");
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

async fn handle_event(
    event: Event,
    state: &mut InMemoryZoneState,
    handle: &lb_zone_sdk::sequencer::SequencerHandle<NodeHttpClient>,
    ready_tx: &mut Option<tokio::sync::oneshot::Sender<()>>,
) {
    match event {
        Event::Ready => handle_ready(state, ready_tx),
        Event::ChannelUpdate {
            orphaned,
            adopted,
            pending,
            invalidated,
            ..
        } => {
            handle_channel_update(state, handle, &orphaned, &adopted, &pending, &invalidated).await;
        }
        Event::TxsFinalized { inscriptions, .. } => {
            finalize_inscriptions(state, &inscriptions, true);
        }
        Event::Published {
            payload,
            checkpoint,
            ..
        } => {
            debug!("Inscription published, checkpoint saved");
            if let Some(msg) = AppMessage::from_bytes(&payload) {
                state.mark_ours(msg.tx_uuid);
                state.apply(msg);
                ui::render_state(state);
                ui::prompt();
            }
            state.save_checkpoint(checkpoint);
        }
        Event::FinalizedInscriptions { inscriptions } => {
            finalize_inscriptions(state, &inscriptions, false);
        }
    }
}

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

async fn handle_channel_update(
    state: &mut InMemoryZoneState,
    handle: &lb_zone_sdk::sequencer::SequencerHandle<NodeHttpClient>,
    orphaned: &[lb_zone_sdk::state::InscriptionInfo],
    adopted: &[lb_zone_sdk::state::InscriptionInfo],
    pending: &[lb_zone_sdk::state::InscriptionInfo],
    invalidated: &[lb_zone_sdk::state::InscriptionInfo],
) {
    if orphaned.is_empty() && adopted.is_empty() && invalidated.is_empty() {
        return;
    }

    debug!(
        orphaned = orphaned.len(),
        adopted = adopted.len(),
        pending = pending.len(),
        invalidated = invalidated.len(),
        "Channel update"
    );

    let to_republish = resolve_conflicts(state, orphaned, adopted, pending, invalidated);
    republish(handle, to_republish).await;

    ui::render_state(state);
    ui::prompt();
}

async fn republish(
    handle: &lb_zone_sdk::sequencer::SequencerHandle<NodeHttpClient>,
    messages: Vec<AppMessage>,
) {
    if messages.is_empty() {
        return;
    }
    info!(count = messages.len(), "Re-publishing after conflict");
    for msg in messages {
        if let Err(e) = handle.publish_message(msg.to_bytes()).await {
            error!("failed to re-publish: {e}");
            break;
        }
    }
}

fn finalize_inscriptions(
    state: &mut InMemoryZoneState,
    inscriptions: &[lb_zone_sdk::state::InscriptionInfo],
    render: bool,
) {
    debug!(count = inscriptions.len(), "Inscriptions finalized");
    let payloads: Vec<Vec<u8>> = inscriptions.iter().map(|i| i.payload.clone()).collect();
    state.finalize(&payloads);
    if render {
        ui::render_state(state);
        ui::prompt();
    }
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
