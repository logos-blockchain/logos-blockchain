use std::{path::PathBuf, process};

use clap::Parser;
use logos_blockchain_cfgsync::server::{CfgSyncConfig, cfgsync_app};
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(about = "CfgSync")]
struct Args {
    config: PathBuf,
}

#[tokio::main]
async fn main() {
    let cli = Args::parse();

    let config = CfgSyncConfig::load_from_file(&cli.config).unwrap_or_else(|err| {
        eprintln!("{err}");
        process::exit(1);
    });

    let port = config.port;
    let app = cfgsync_app(config.into());

    println!("Server running on http://0.0.0.0:{port}");
    let listener = TcpListener::bind(&format!("0.0.0.0:{port}")).await.unwrap();

    axum::serve(listener, app).await.unwrap();
}
