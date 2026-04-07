use lb_core::{header::HeaderId, mantle::gas::GasOverflow};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum WalletError {
    #[error("Requested wallet state for unknown block: {0}")]
    UnknownBlock(HeaderId),
    #[error("Wallet does not have enough funds, available={available}")]
    InsufficientFunds { available: u64 },
    #[error(transparent)]
    GasOverflow(#[from] GasOverflow),
}
