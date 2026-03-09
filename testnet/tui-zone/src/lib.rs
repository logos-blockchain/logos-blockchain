use std::{fs, io::Write as _, path::Path, time::Duration};

use clap::Parser;
use lb_common_http_client::BasicAuthCredentials;
use lb_core::mantle::ops::channel::ChannelId;
use lb_key_management_system_service::keys::{ED25519_SECRET_KEY_SIZE, Ed25519Key};
use lb_zone_sdk::sequencer::{Error, SequencerCheckpoint, ZoneSequencer};
use reqwest::Url;
use tokio::task::JoinHandle;

#[derive(Parser, Debug)]
#[command(about = "Terminal UI zone sequencer - publish text inscriptions")]
pub struct InscribeArgs {
    /// Logos blockchain node HTTP endpoint
    #[arg(long, default_value = "http://localhost:8080", env = "NODE_URL")]
    node_url: String,

    /// Path to the signing key file (created if it doesn't exist)
    #[arg(long, default_value = "sequencer.key", env = "KEY_PATH")]
    key_path: String,

    /// Path to the checkpoint file for crash recovery
    #[arg(long, default_value = "sequencer.checkpoint", env = "CHECKPOINT_PATH")]
    checkpoint_path: String,
}

fn save_checkpoint(path: &Path, checkpoint: &SequencerCheckpoint) {
    let data = serde_json::to_vec(checkpoint).expect("failed to serialize checkpoint");
    fs::write(path, data).expect("failed to write checkpoint file");
}

fn load_checkpoint(path: &Path) -> Option<SequencerCheckpoint> {
    if !path.exists() {
        return None;
    }
    let data = fs::read(path).expect("failed to read checkpoint file");
    Some(serde_json::from_slice(&data).expect("failed to deserialize checkpoint"))
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

/// A simple spinner that prints dots while an async operation is in progress.
struct Spinner {
    handle: Option<JoinHandle<()>>,
    cancel: tokio::sync::watch::Sender<bool>,
}

impl Spinner {
    fn start(message: &str) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let msg = message.to_owned();

        let handle = tokio::spawn(async move {
            #[expect(clippy::non_ascii_literal, reason = "UI animation")]
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut i = 0;
            loop {
                print!("\r  {} {}", frames[i % frames.len()], msg);
                drop(std::io::stdout().flush());
                i += 1;

                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(80)) => {}
                    _ = cancel_rx.changed() => {
                        // Clear the spinner line
                        print!("\r{}\r", " ".repeat(msg.len() + 6));
                        drop(std::io::stdout().flush());
                        return;
                    }
                }
            }
        });

        Self {
            handle: Some(handle),
            cancel: cancel_tx,
        }
    }

    async fn stop(mut self) {
        let _ = self.cancel.send(true);
        if let Some(handle) = self.handle.take() {
            drop(handle.await);
        }
    }
}

#[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
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

    let checkpoint_path = Path::new(&args.checkpoint_path);
    let checkpoint = load_checkpoint(checkpoint_path);
    if checkpoint.is_some() {
        println!("  Restored checkpoint from {}", args.checkpoint_path);
    }

    let sequencer = ZoneSequencer::init(
        channel_id,
        signing_key,
        node_url,
        Some(BasicAuthCredentials::new(
            "strode".to_owned(),
            Some("SzH3RP7zdVQs8LCb".to_owned()),
        )),
        checkpoint,
    );

    println!();
    println!("Type a message and press Enter to publish it as a zone block.");
    println!("Press Ctrl-D or type an empty line to exit.");
    println!();

    let stdin = std::io::stdin();
    let mut line = String::new();

    loop {
        print!("> ");
        std::io::stdout().flush().expect("failed to flush stdout");

        line.clear();
        let bytes_read = stdin.read_line(&mut line).expect("failed to read line");

        if bytes_read == 0 {
            println!();
            break;
        }

        let msg = line.trim_end();
        if msg.is_empty() {
            break;
        }

        let data = msg.as_bytes().to_vec();

        let spinner = Spinner::start("Publishing zone block, waiting for inclusion...");
        let result = sequencer.publish(data.clone()).await;
        spinner.stop().await;

        match result {
            Ok(result) => {
                let tx_hash: [u8; 32] = result.inscription_id.into();
                println!("  ✓ Included: {}", hex::encode(tx_hash));
                save_checkpoint(checkpoint_path, &result.checkpoint);
            }
            Err(Error::InclusionTimeout) => {
                #[expect(clippy::non_ascii_literal, reason = "UI")]
                {
                    println!(
                        "  ✗ Timed out waiting for inclusion. The inscription may still be included later."
                    );
                }
            }
            Err(Error::Conflict { blobs }) => {
                println!();
                if !blobs.is_empty() {
                    println!(
                        "  ⚠ {} blob(s) appeared on the channel since your last publish:",
                        blobs.len()
                    );
                    for (i, blob) in blobs.iter().enumerate() {
                        match core::str::from_utf8(blob) {
                            Ok(text) => {
                                println!("    [{}] \"{}\"", i + 1, text);
                            }
                            Err(_) => {
                                println!("    [{}] ({} bytes, non-UTF8)", i + 1, blob.len());
                            }
                        }
                    }
                }
                println!();
                print!("  Re-publish your message on the new chain head? [Y/n] ");
                std::io::stdout().flush().expect("failed to flush stdout");

                let mut answer = String::new();
                stdin.read_line(&mut answer).expect("failed to read answer");

                let answer = answer.trim();
                if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
                    let spinner = Spinner::start("Re-publishing, waiting for inclusion...");
                    let retry_result = sequencer.publish(data).await;
                    spinner.stop().await;

                    match retry_result {
                        Ok(result) => {
                            let tx_hash: [u8; 32] = result.inscription_id.into();
                            println!("  ✓ Included: {}", hex::encode(tx_hash));
                            save_checkpoint(checkpoint_path, &result.checkpoint);
                        }
                        Err(e) => {
                            println!("  ✗ Error on retry: {e}");
                        }
                    }
                } else {
                    println!("  Aborted. Your message was not published.");
                }
            }
            Err(e) => {
                println!("  ✗ Error: {e}");
            }
        }
    }

    println!("Goodbye!");
}
