use lb_core::{
    header::HeaderId,
    mantle::{
        gas::GasOverflow,
        ledger::{InputsError, OutputsError},
        transactions::builder::TxBuilderError,
    },
};
use lb_utils::bounded::BoundedError;
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
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
    #[error(transparent)]
    InputsError(#[from] InputsError),
    #[error(transparent)]
    OutputsError(#[from] OutputsError),
}

impl From<TxBuilderError> for WalletError {
    fn from(error: TxBuilderError) -> Self {
        Self::TxBuilder(error.to_string())
    }
}
