/// Chain-derived wallet accounting.
pub mod accounting;
/// Block streaming helpers for scanner tasks.
pub mod block_source;
/// Scanner runtime configuration.
pub mod config;
/// Scanner error types.
pub mod error;
/// Fork-group scanner configuration assembly.
pub mod group;
/// Scanner task runtime and catch-up waiting.
pub mod runner;
/// Shared scanner status state.
pub mod state;

/// Scanner startup seed.
pub use config::ScannerSeed;
/// Build scanner configs for all fork groups with tracked wallets.
pub use group::build_fork_group_scanner_configs;
/// Scanner task handles and startup/wait helpers.
pub use runner::{WalletScannerRuntime, start_wallet_scanners, wait_for_scanner_catch_up};
/// Shared scanner status types.
pub use state::{SharedWalletScannerState, WalletScannerState};
