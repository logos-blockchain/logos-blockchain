//! Shared error type for wallet transaction preparation.

use lb_core::mantle::{NoteId, VerificationError, gas::GasOverflow};
use lb_wallet::WalletError;
use lb_zksign::ZkSignError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WalletTransactionError {
    #[error(transparent)]
    Funding(#[from] WalletError),
    #[error("missing funding UTXO for transfer input {note_id:?}")]
    MissingFundingInput { note_id: NoteId },
    #[error("missing signing key for transfer input {note_id:?}")]
    MissingSigningKey { note_id: NoteId },
    #[error(transparent)]
    Signing(#[from] ZkSignError),
    #[error(transparent)]
    Verification(#[from] VerificationError),
    #[error(transparent)]
    Gas(#[from] GasOverflow),
    #[error("wallet transaction output total overflowed u64")]
    OutputTotalOverflow,
}
