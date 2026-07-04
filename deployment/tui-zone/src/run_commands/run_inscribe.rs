use std::{collections::HashSet, path::Path};

use lb_core::mantle::ops::channel::inscribe::Inscription;
use lb_zone_sdk::{
    CommonHttpClient,
    adapter::NodeHttpClient,
    sequencer::{
        ChannelUpdate, Event, FinalizedOp, FinalizedTx, InscriptionInfo, OrphanedTx, ZoneSequencer,
    },
};
use reqwest::Url;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::{
    cli::NodeKeyArgs,
    message::AppMessage,
    run_commands::utils,
    state::{InMemoryZoneState, ZoneState as _},
    ui,
};

#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
/// Run the interactive inscription TUI.
pub async fn run_inscribe(args: NodeKeyArgs) {
    let node_url: Url = args.node_url.parse().expect("invalid node URL");
    let signing_key = utils::load_or_create_signing_key(Path::new(&args.key_path));
    let channel_id = utils::resolve_channel_id(&args).expect("invalid channel ID");

    println!("TUI Zone Sequencer");
    println!("  Node:       {node_url}");
    println!("  Key:        {}", args.key_path);
    println!("  Channel ID: {}", hex::encode(channel_id.as_ref()));
    println!();

    let node = NodeHttpClient::new(CommonHttpClient::new(None), node_url);
    let channel_exists = utils::query_channel_exists(&node, channel_id).await;
    let mut state =
        InMemoryZoneState::for_channel(channel_id, channel_exists).expect("invalid checkpoint");
    let checkpoint = state.load_checkpoint().cloned();

    let mut sequencer = ZoneSequencer::init(channel_id, signing_key, node, checkpoint);
    let view_rx = sequencer.subscribe_channel_view();

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let mut stdin_rx = spawn_stdin_reader(ready_rx);
    let mut ready_tx = Some(ready_tx);

    println!("Bootstrapping sequencer...");

    loop {
        tokio::select! {
            event = sequencer.next_event() => {
                state.set_channel_view(view_rx.borrow().clone());
                handle_event(event, &mut state, &mut sequencer, &mut ready_tx);
            }

            input = stdin_rx.recv() => {
                let Some(text) = input else {
                    println!();
                    break;
                };

                let msg = AppMessage::new(text);
                debug!(tx_uuid = %msg.tx_uuid, text = %msg.text, "Publishing message");
                let Ok(inscription) = Inscription::try_from(msg.to_bytes()) else {
                    error!("Message is too large to fit in an inscription");
                    continue;
                };
                match sequencer.handle().publish(inscription) {
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

            _ = tokio::signal::ctrl_c() => {
                println!();
                break;
            }
        }
    }

    println!("Goodbye!");
}

fn handle_event(
    event: Event,
    state: &mut InMemoryZoneState,
    sequencer: &mut ZoneSequencer<NodeHttpClient>,
    ready_tx: &mut Option<tokio::sync::oneshot::Sender<()>>,
) {
    match event {
        Event::Ready => handle_ready(state, ready_tx),
        Event::BlocksProcessed {
            checkpoint,
            channel_update,
            finalized,
        } => {
            // Pin finalized payloads before the re-publish dedup below.
            apply_finalized(&finalized, state);
            apply_channel_update(channel_update, state, sequencer);
            state.save_checkpoint(checkpoint);
        }
        Event::MempoolPending(..) | Event::TurnNotification { .. } => {}
    }
}

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
    if inscriptions.is_empty() {
        return;
    }
    state.on_finalized(&inscriptions);
    ui::render_state(state);
    ui::prompt();
}

fn apply_channel_update(
    update: ChannelUpdate,
    state: &mut InMemoryZoneState,
    sequencer: &mut ZoneSequencer<NodeHttpClient>,
) {
    let ChannelUpdate { orphaned, adopted } = update;
    if orphaned.is_empty() {
        return;
    }
    // Dedup by payload (carries a unique tx_uuid): an orphan already back on
    // the channel reappears in `adopted`, so don't republish it.
    let adopted_payloads: HashSet<&[u8]> = adopted.iter().map(|i| i.payload.as_slice()).collect();
    for entry in &orphaned {
        handle_orphan(state, sequencer, entry, &adopted_payloads);
    }
}

fn handle_orphan(
    state: &mut InMemoryZoneState,
    sequencer: &mut ZoneSequencer<NodeHttpClient>,
    entry: &OrphanedTx,
    adopted_payloads: &HashSet<&[u8]>,
) {
    let OrphanedTx::Inscription(info) = entry else {
        debug!("ignoring atomic-withdraw orphan; TUI does not publish bundles");
        return;
    };
    if adopted_payloads.contains(info.payload.as_slice()) {
        debug!(msg_id = %hex::encode(info.this_msg.as_ref()), "orphan already on channel; not republishing");
        return;
    }
    if state.is_finalized(info.payload.as_slice()) {
        debug!(msg_id = %hex::encode(info.this_msg.as_ref()), "orphan already finalized; not republishing");
        return;
    }
    republish_orphan(state, sequencer, info);
}

fn republish_orphan(
    state: &mut InMemoryZoneState,
    sequencer: &mut ZoneSequencer<NodeHttpClient>,
    info: &InscriptionInfo,
) {
    debug!(msg_id = %hex::encode(info.this_msg.as_ref()), "Auto-republishing orphan");
    match sequencer.handle().publish(info.payload.clone()) {
        Ok((_, checkpoint)) => state.save_checkpoint(checkpoint),
        Err(e) => error!("failed to auto-republish: {e}"),
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
