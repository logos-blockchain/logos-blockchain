use std::{path::PathBuf, process};

use clap::Parser;
use logos_blockchain_cfgsync::{
    CfgsyncMode,
    server::{CfgSyncConfig, cfgsync_app},
};
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
    #[arg(short, long, env = "CFG_SERVER_MODE")]
    mode: Option<CfgsyncMode>,
    #[arg(short, long, env = "CFG_SERVER_STORAGE_PATH")]
    storage_path: Option<PathBuf>,
    #[arg(short, long, env = "ENTROPY_FILE")]
    entropy_file: Option<PathBuf>,
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

    if let Some(storage_path) = cli.storage_path {
        config.deployment_settings_storage_path = storage_path;
    }

    if let Some(entropy_file) = cli.entropy_file {
        config.entropy_file = entropy_file;
    }

    if let Some(mode) = cli.mode {
        config.mode = mode;
    }

    let port = config.port;
    let mode = config.mode;
    let app = cfgsync_app(config.into(), mode);

    println!("Server running on http://0.0.0.0:{port}");
    let listener = TcpListener::bind(&format!("0.0.0.0:{port}")).await.unwrap();

    axum::serve(listener, app).await.unwrap();
}
