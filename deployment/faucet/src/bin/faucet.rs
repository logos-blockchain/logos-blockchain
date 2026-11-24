use std::{sync::Arc, time::Duration};

use clap::Parser;
use lb_groth16::fr_from_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;
use logos_blockchain_faucet::{
    faucet::{Faucet, run_worker},
    server::{FaucetServerState, faucet_app},
};
use reqwest::Url;
use tokio::{net::TcpListener, sync::mpsc};

const DRIP_QUEUE_CAPACITY: usize = 1024;

#[derive(Parser, Debug)]
#[command(about = "Faucet")]
struct Args {
    #[arg(short, long, default_value_t = 6000)]
    port: u16,
    #[arg(short, long)]
    node_base_url: Url,
    /// Hex-encoded faucet public key.
    #[arg(long)]
    faucet_pk: String,
    #[arg(long)]
    drip_amount: u64,
    /// Minimum number of seconds between drips for the same recipient key.
    #[arg(long, default_value_t = 300)]
    cooldown_secs: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let faucet_pk = parse_pk(&args.faucet_pk);

    println!("Faucet PK: {faucet_pk:?}");

    let faucet = Arc::new(
        Faucet::new(args.node_base_url, faucet_pk, args.drip_amount)
            .expect("faucet should be created"),
    );

    let (queue, requests) = mpsc::channel(DRIP_QUEUE_CAPACITY);
    tokio::spawn(run_worker(Arc::clone(&faucet), requests));

    let state = Arc::new(FaucetServerState::new(
        queue,
        Duration::from_secs(args.cooldown_secs),
    ));
    let app = faucet_app(state);

    println!("Faucet server running on http://0.0.0.0:{}", args.port);
    let listener = TcpListener::bind(&format!("0.0.0.0:{}", args.port))
        .await
        .unwrap();

    axum::serve(listener, app).await.unwrap();
}

fn parse_pk(hex_str: &str) -> ZkPublicKey {
    let pk_bytes = hex::decode(hex_str).expect("faucet-pk must be valid hex");
    ZkPublicKey::new(fr_from_bytes(&pk_bytes).expect("faucet-pk must be a valid field element"))
}
