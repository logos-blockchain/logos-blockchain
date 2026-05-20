pub mod benchmarks;
pub mod common;
pub mod cucumber;
use std::{env, sync::LazyLock};

/// Global flag indicating whether debug tracing configuration is enabled to
/// send traces to local grafana stack.
pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(|| {
    env::var("LOGOS_BLOCKCHAIN_TESTS_TRACING").is_ok_and(|val| val.eq_ignore_ascii_case("true"))
});

/// The default path to the node binary for debug builds.
pub const BIN_PATH_DEBUG: &str = "../target/debug/logos-blockchain-node";
/// The default path to the node binary for release builds.
pub const BIN_PATH_RELEASE: &str = "../target/release/logos-blockchain-node";
