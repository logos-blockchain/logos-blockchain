mod api;
mod config;
mod ctrl_c;
mod sequencer;

use std::sync::Arc;

use demo_sequencer::db::AccountDb;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::{api::create_router, config::Config, ctrl_c::listen_for_sigint, sequencer::Sequencer};

fn print_banner() {
    const BLUE: &str = "\x1b[38;5;39m";
    const RESET: &str = "\x1b[0m";
    println!(
        r"
{BLUE} __  __                 _____ _           _
|  \/  | ___ _ __ ___  / ____| |__   __ _(_)_ __
| |\/| |/ _ \ '_ ` _ \| |    | '_ \ / _` | | '_ \
| |  | |  __/ | | | | | |____| | | | (_| | | | | |
|_|  |_|\___|_| |_| |_|\_____|_| |_|\__,_|_|_| |_|
 ____
/ ___|  ___  __ _ _   _  ___ _ __   ___ ___ _ __
\___ \ / _ \/ _` | | | |/ _ \ '_ \ / __/ _ \ '__|
 ___) |  __/ (_| | |_| |  __/ | | | (_|  __/ |
|____/ \___|\__, |\__,_|\___|_| |_|\___\___|_|
               |_|{RESET}
"
    );
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    print_banner();
    info!("MemChainSequencer starting up...");

    // Load configuration
    let config = Config::from_env();
    info!("Configuration");
    info!("  HTTP API:       {}", config.listen_addr);
    info!("  Nomos Node:     {}", config.node_endpoint);
    info!("  Database:       {}", config.db_path);
    info!("  Channel ID:     {}", config.channel_id);
    info!("  Initial funds:  {} tokens", config.initial_balance);

    // Initialize database
    let db = match AccountDb::new(&config.db_path, config.initial_balance) {
        Ok(db) => db,
        Err(e) => {
            error!("Database initialization failed: {e}");
            std::process::exit(1);
        }
    };
    info!("Database ready");

    // Initialize sequencer
    let sequencer = match Sequencer::new(
        db,
        &config.node_endpoint,
        &config.signing_key_path,
        &config.channel_id,
        config.node_auth_username,
        config.node_auth_password,
    ) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!("Sequencer initialization failed: {e}");
            std::process::exit(1);
        }
    };
    info!("Sequencer ready");

    // Setup cancellation token for graceful shutdown
    let cancellation_token = CancellationToken::new();
    listen_for_sigint(cancellation_token.clone());

    // Spawn background processing loop
    let sequencer_clone = Arc::clone(&sequencer);
    tokio::spawn(async move {
        sequencer_clone.run_processing_loop().await;
    });
    info!("Background processor started");

    // Create HTTP router
    let app = create_router(sequencer);

    // Start HTTP server
    info!("MemChainSequencer listening on {}", config.listen_addr);
    let listener = match tokio::net::TcpListener::bind(config.listen_addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind: {e}");
            std::process::exit(1);
        }
    };

    let shutdown_signal = cancellation_token.cancelled_owned();
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
    {
        error!("Server error: {e}");
        std::process::exit(1);
    }
}
