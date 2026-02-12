use std::{fs, io::Write as _, path::Path};

use clap::Parser;
use lb_common_http_client::Slot;
use lb_core::{
    header::HeaderId,
    mantle::{
        SignedMantleTx,
        ops::channel::{ChannelId, MsgId},
        tx::TxHash,
    },
};
use lb_key_management_system_service::keys::{ED25519_SECRET_KEY_SIZE, Ed25519Key};
use lb_zone_sdk::sequencer::{SequencerCheckpoint, ZoneSequencer};
use reqwest::Url;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "tui-sequencer", about = "Terminal UI zone sequencer")]
struct Cli {
    /// Logos blockchain node HTTP endpoint
    #[arg(long, default_value = "http://localhost:18080", env = "NODE_URL")]
    node_url: String,

    /// Path to the signing key file (created if it doesn't exist)
    #[arg(long, default_value = "sequencer.key", env = "KEY_PATH")]
    key_path: String,

    /// Path to the checkpoint file for crash recovery
    #[arg(long, default_value = "sequencer.checkpoint", env = "CHECKPOINT_PATH")]
    checkpoint_path: String,
}

#[derive(Serialize, Deserialize)]
struct CheckpointFile {
    last_msg_id: MsgId,
    pending_txs: Vec<(TxHash, SignedMantleTx)>,
    lib: HeaderId,
    lib_slot: Slot,
}

impl From<SequencerCheckpoint> for CheckpointFile {
    fn from(cp: SequencerCheckpoint) -> Self {
        Self {
            last_msg_id: cp.last_msg_id,
            pending_txs: cp.pending_txs,
            lib: cp.lib,
            lib_slot: cp.lib_slot,
        }
    }
}

impl From<CheckpointFile> for SequencerCheckpoint {
    fn from(f: CheckpointFile) -> Self {
        Self {
            last_msg_id: f.last_msg_id,
            pending_txs: f.pending_txs,
            lib: f.lib,
            lib_slot: f.lib_slot,
        }
    }
}

fn save_checkpoint(path: &Path, checkpoint: &SequencerCheckpoint) {
    let file: CheckpointFile = CheckpointFile {
        last_msg_id: checkpoint.last_msg_id,
        pending_txs: checkpoint.pending_txs.clone(),
        lib: checkpoint.lib,
        lib_slot: checkpoint.lib_slot,
    };
    let data = serde_json::to_vec(&file).expect("failed to serialize checkpoint");
    fs::write(path, data).expect("failed to write checkpoint file");
}

fn load_checkpoint(path: &Path) -> Option<SequencerCheckpoint> {
    if !path.exists() {
        return None;
    }
    let data = fs::read(path).expect("failed to read checkpoint file");
    let file: CheckpointFile =
        serde_json::from_slice(&data).expect("failed to deserialize checkpoint");
    Some(file.into())
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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let node_url: Url = cli.node_url.parse().expect("invalid node URL");
    let signing_key = load_or_create_signing_key(Path::new(&cli.key_path));
    let channel_id = ChannelId::from(signing_key.public_key().to_bytes());

    println!("TUI Zone Sequencer");
    println!("  Node:       {node_url}");
    println!("  Key:        {}", cli.key_path);
    println!("  Channel ID: {}", hex::encode(channel_id.as_ref()));
    println!();

    let checkpoint_path = Path::new(&cli.checkpoint_path);
    let checkpoint = load_checkpoint(checkpoint_path);
    if checkpoint.is_some() {
        println!("  Restored checkpoint from {}", cli.checkpoint_path);
    }

    let sequencer = ZoneSequencer::init(channel_id, signing_key, node_url, None, checkpoint);

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
            // EOF
            println!();
            break;
        }

        let msg = line.trim_end();
        if msg.is_empty() {
            break;
        }

        match sequencer.publish(msg.as_bytes().to_vec()).await {
            Ok(result) => {
                let tx_hash: [u8; 32] = result.inscription_id.into();
                println!("  published: {}", hex::encode(tx_hash));
                save_checkpoint(checkpoint_path, &result.checkpoint);
            }
            Err(e) => {
                println!("  error: {e}");
            }
        }
    }

    println!("Goodbye!");
}
