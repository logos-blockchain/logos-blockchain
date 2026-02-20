use std::sync::Arc;

use clap::Parser;
use lb_groth16::fr_from_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;
use logos_blockchain_faucet::{faucet::Faucet, server::faucet_app};
use reqwest::Url;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(about = "Faucet")]
struct Args {
    #[arg(short, long, default_value_t = 6000)]
    port: u16,
    #[arg(short, long)]
    node_base_url: Url,
    /// Hex-encoded faucet public key
    #[arg(long)]
    faucet_pk: String,
    #[arg(short, long)]
    drip_amount: u64,
}

#[tokio::main]
async fn main() {
    let Args {
        port,
        drip_amount,
        faucet_pk,
        node_base_url,
    } = Args::parse();

    let pk_bytes = hex::decode(&faucet_pk).expect("faucet-pk must be valid hex");
    let faucet_pk = ZkPublicKey::new(
        fr_from_bytes(&pk_bytes).expect("faucet-pk must be a valid field element"),
    );

    println!("Faucet PK: {faucet_pk:?}");

    let faucet = Arc::new(
        Faucet::new(node_base_url, faucet_pk, drip_amount).expect("faucet should be created"),
    );
    let app = faucet_app(faucet);

    println!("Faucet server running on http://0.0.0.0:{port}");
    let listener = TcpListener::bind(&format!("0.0.0.0:{port}")).await.unwrap();

    axum::serve(listener, app).await.unwrap();
}
