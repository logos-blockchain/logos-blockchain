use lb_core::{
    header::HeaderId,
    mantle::{
        gas::GasOverflow,
        ledger::{InputsError, OutputsError},
        transactions::builder::TxBuilderError,
    },
};
use lb_mmr::MerklePathError;
use lb_utils::bounded::BoundedError;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum WalletError {
    #[error("Requested wallet state for unknown block: {0}")]
    UnknownBlock(HeaderId),
    /// `required` is the spendable value that still funds a later attempt when
    /// the gap arrives as one new note. The quote includes that extra input
    /// and a change output, at this attempt's gas price.
    #[error("Wallet does not have enough funds, available={available}, required={required}")]
    InsufficientFunds { available: u64, required: u64 },
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
    #[error(transparent)]
    InvalidVoucherPath(#[from] MerklePathError),
    #[error("Voucher MMR is full")]
    VoucherMmrFull,
}

impl From<TxBuilderError> for WalletError {
    fn from(error: TxBuilderError) -> Self {
        Self::TxBuilder(error.to_string())
    }
}
