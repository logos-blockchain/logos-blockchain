use std::{path::PathBuf, sync::Arc};

use clap::Parser;
use logos_blockchain_faucet::{faucet::Faucet, server::faucet_app};
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(about = "Faucet")]
struct Args {
    #[arg(short, long, default_value_t = 6000)]
    port: u16,
    #[arg(short, long)]
    node_config_path: PathBuf,
    #[arg(short, long)]
    drip_rate: u64,
}

#[tokio::main]
async fn main() {
    let Args {
        port,
        node_config_path,
        drip_rate,
    } = Args::parse();

    let faucet =
        Arc::new(Faucet::new(&node_config_path, drip_rate).expect("faucet should be created"));
    let app = faucet_app(faucet);

    println!("Faucet server running on http://0.0.0.0:{port}");
    let listener = TcpListener::bind(&format!("0.0.0.0:{port}")).await.unwrap();

    axum::serve(listener, app).await.unwrap();
}
