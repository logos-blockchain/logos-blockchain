use lb_core::{
    header::HeaderId,
    mantle::{gas::GasOverflow, tx_builder::TxBuilderError},
};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum WalletError {
    #[error("Requested wallet state for unknown block: {0}")]
    UnknownBlock(HeaderId),
    #[error("Wallet does not have enough funds, available={available}")]
    InsufficientFunds { available: u64 },
    #[error(transparent)]
    GasOverflow(#[from] GasOverflow),
    #[error("Transaction builder error: {0}")]
    TxBuilder(String),
}

impl From<TxBuilderError> for WalletError {
    fn from(error: TxBuilderError) -> Self {
        Self::TxBuilder(error.to_string())
    }
}
