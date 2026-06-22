/// The TUI sequencer command runner module.
mod driver;
/// Channel balance query command runner.
pub mod run_balance;
/// Channel configuration command runner.
pub mod run_config;
/// Deposit command runner.
pub mod run_deposit;
/// Interactive inscription command runner.
pub mod run_inscribe;
/// Local sequencer key generation command runner.
pub mod run_keygen;
/// Withdrawal command runners.
pub mod run_withdraw;
/// Transaction signing command runner.
pub(crate) mod types;
/// Utility functions for the TUI sequencer command runners.
mod utils;

#[cfg(test)]
mod unit_tests;

pub const ZONE_DEPOSIT_SUBMISSION: &str = "zone_deposit_submission";
/// The TUI sequencer prefix for all config intent files to ensure they are
/// easily identifiable.
pub const ZONE_CONFIG_INTENT: &str = "zone_config_intent";
/// The TUI sequencer prefix for all config signature files to ensure they are
/// easily identifiable.
pub const ZONE_CONFIG_SIGNATURE: &str = "zone_config_signature";
/// The TUI sequencer prefix for all signed config transaction files to ensure
/// they are easily identifiable.
pub const ZONE_SIGNED_CONFIG: &str = "zone_signed_config";

/// The TUI sequencer prefix for all withdraw intent files to ensure they are
/// easily identifiable.
pub const ZONE_WITHDRAW_INTENT: &str = "zone_withdraw_intent";

/// The TUI sequencer prefix for all withdraw signature files to ensure they are
/// easily identifiable.
pub const ZONE_WITHDRAW_SIGNATURE: &str = "zone_withdraw_signature";
/// The TUI sequencer prefix for all signed transaction files to ensure they are
/// easily identifiable.
pub const ZONE_SIGNED_TRANSACTION: &str = "zone_signed_transaction";
/// The TUI sequencer prefix for all funds export files to ensure they are
/// easily identifiable.
pub const ZONE_WALLET_FUNDS_EXPORT: &str = "zone_wallet_funds_export";
/// The TUI sequencer file transfer version to ensure compatibility of transfer
/// files.
pub const ZONE_FILE_TRANSFER_VERSION: u8 = 1;
