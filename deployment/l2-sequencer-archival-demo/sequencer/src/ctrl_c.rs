use owo_colors::OwoColorize as _;
use tokio::signal::ctrl_c;
use tokio_util::sync::CancellationToken;

pub fn listen_for_sigint(cancellation_token: CancellationToken) {
    tokio::spawn(async move {
        ctrl_c().await.unwrap();
        println!("\n  {} Graceful shutdown initiated...", "\u{1f6d1}".red());
        cancellation_token.cancel();
    });
}
