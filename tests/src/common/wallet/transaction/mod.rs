//! Wallet transaction preparation, funding, and signing.
//!
//! This module is the reusable wallet transaction API. It owns transaction
//! intents and prepared/signed submissions; Cucumber-only submission flow lives
//! in `crate::cucumber::wallet::submissions`.

mod builder_funding;
mod error;
mod intent;
mod prepare;
mod prepared;
mod signed;
mod signing;

pub use builder_funding::{fund_builder_from_wallet_source, wallet_state_from_utxos};
pub use error::WalletTransactionError;
pub use intent::WalletTransactionIntent;
pub use prepare::{
    PreparedWalletTransactionWorkItem, finalize_prepared_wallet_transaction,
    prepare_wallet_transaction, prepare_wallet_transaction_work_item,
};
pub use prepared::PreparedWalletTransaction;
pub use signed::SignedWalletTransaction;
