use std::{path::PathBuf, process};

use clap::Parser;
use logos_blockchain_cfgsync::server::{CfgSyncConfig, cfgsync_app};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::net::TcpListener;

fn parse_rfc3339(s: &str) -> Result<OffsetDateTime, String> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| format!("Invalid RFC3339 format (2026-02-12T04:45:00Z): {e}"))
}

#[derive(Parser, Debug)]
#[command(about = "CfgSync")]
struct Args {
    config: PathBuf,
    #[arg(short, long, env = "CHAIN_START_TIME", value_parser = parse_rfc3339)]
    chain_start_time: Option<OffsetDateTime>,
}

#[tokio::main]
async fn main() {
    let cli = Args::parse();

    let mut config = CfgSyncConfig::load_from_file(&cli.config).unwrap_or_else(|err| {
        eprintln!("{err}");
        process::exit(1);
    });

    if let Some(chain_start_time) = cli.chain_start_time {
        config.chain_start_time = Some(chain_start_time);
    }

    let port = config.port;
    let app = cfgsync_app(config.into());

    println!("Server running on http://0.0.0.0:{port}");
    let listener = TcpListener::bind(&format!("0.0.0.0:{port}")).await.unwrap();

    axum::serve(listener, app).await.unwrap();
}
