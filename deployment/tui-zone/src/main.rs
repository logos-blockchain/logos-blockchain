use clap::Parser as _;
use logos_blockchain_tui_zone::cli::{Cli, run_cli};
use tracing_subscriber::{
    Layer as _, filter::LevelFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

#[tokio::main]
async fn main() {
    let file_appender = tracing_appender::rolling::daily("logs", "tui-zone.log");
    let (log_writer, _log_guard) = tracing_appender::non_blocking(file_appender);

    let console_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .with_filter(LevelFilter::WARN);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(log_writer)
        .with_ansi(false)
        .with_filter(LevelFilter::DEBUG);

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .init();

    let args = Cli::parse();
    if let Err(error) = run_cli(args).await {
        eprintln!("tui-zone command failed: {error}");
        std::process::exit(1);
    }
}
