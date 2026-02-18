use std::sync::Arc;

use clap::Parser;
use lb_cfgsync::config::host_to_id;
use lb_key_management_system_keys::keys::ZkKey;
use logos_blockchain_faucet::{faucet::Faucet, server::faucet_app};
use num_bigint::BigUint;
use reqwest::Url;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(about = "Faucet")]
struct Args {
    #[arg(short, long, default_value_t = 6000)]
    port: u16,
    #[arg(short, long)]
    node_base_url: Url,
    #[arg(short = 'i', long)]
    host_identifier: String,
    #[arg(short, long)]
    drip_amount: u64,
}

#[tokio::main]
async fn main() {
    let Args {
        port,
        drip_amount,
        host_identifier,
        node_base_url,
    } = Args::parse();

    let host_id = host_to_id(&host_identifier);
    println!("Faucet for {host_identifier} ({host_id:?})");

    let sk_faucet_data = derive_key_material(b"fc", &host_id);
    let sk_faucet = ZkKey::from(BigUint::from_bytes_le(&sk_faucet_data));

    let faucet = Arc::new(
        Faucet::new(node_base_url, sk_faucet.to_public_key(), drip_amount)
            .expect("faucet should be created"),
    );
    let app = faucet_app(faucet);

    println!("Faucet server running on http://0.0.0.0:{port}");
    let listener = TcpListener::bind(&format!("0.0.0.0:{port}")).await.unwrap();

    axum::serve(listener, app).await.unwrap();
}

fn derive_key_material(prefix: &[u8], id_bytes: &[u8]) -> [u8; 16] {
    let mut sk_data = [0; 16];
    let prefix_len = prefix.len();

    sk_data[..prefix_len].copy_from_slice(prefix);
    let bytes_to_copy = std::cmp::min(16 - prefix_len, id_bytes.len());
    sk_data[prefix_len..prefix_len + bytes_to_copy].copy_from_slice(&id_bytes[..bytes_to_copy]);

    sk_data
}
