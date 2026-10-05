use std::process::ExitCode;

use logos_blockchain_tests::cucumber::{deployment::LocalImplementation, runner};

#[tokio::main]
async fn main() -> ExitCode {
    runner::run(LocalImplementation::Logos).await
}
