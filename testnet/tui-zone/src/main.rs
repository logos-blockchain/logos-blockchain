use clap::Parser as _;
use logos_blockchain_tui_zone::{InscribeArgs, run};

#[tokio::main]
async fn main() {
    let args = InscribeArgs::parse();
    run(args).await;
}
