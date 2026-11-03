use lb_core::{
    header::HeaderId,
    mantle::{
        gas::GasOverflow, note_set::NoteSetIndex, ops::channel::ChannelId,
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
    #[error("Wallet does not have enough funds, available={available}")]
    InsufficientFunds { available: u64 },
    #[error(transparent)]
    GasOverflow(#[from] GasOverflow),
    #[error("Transaction builder error: {0}")]
    TxBuilder(String),
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
    #[error("The commitment MMR of note set {0} is full")]
    MmrFull(NoteSetIndex),
    #[error(transparent)]
    InvalidPath(#[from] MerklePathError),
    #[error("Operation {op_index} needs a note opening the transaction builder does not carry")]
    MissingOpening { op_index: usize },
    #[error("Operation {op_index} spends a note the wallet has no path for")]
    MissingInputPath { op_index: usize },
    #[error("Operation {op_index} references a root the wallet is not at")]
    UnknownRoot { op_index: usize },
    #[error("Operation {op_index} names a nullifier its opening does not derive")]
    NullifierMismatch { op_index: usize },
    #[error("Channel {0:?} is not in the witness context")]
    UnknownChannel(ChannelId),
    #[error("Channel steps are proven by their holders")]
    UnsupportedSteps { op_index: usize },
}

impl From<TxBuilderError> for WalletError {
    fn from(error: TxBuilderError) -> Self {
        Self::TxBuilder(error.to_string())
    }
}
