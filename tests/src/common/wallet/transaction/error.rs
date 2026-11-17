//! Shared error type for wallet transaction preparation.

use lb_core::mantle::{
    VerificationError, gas::GasOverflow, notes::NoteNullifier, transactions::TxBuilderError,
};
use lb_key_management_system_service::keys::{ZkPublicKey, errors::KeyError};
use lb_utils::bounded::BoundedError;
use lb_wallet::WalletError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WalletTransactionError {
    #[error(transparent)]
    Funding(#[from] WalletError),
    #[error("missing funding note for transfer input {nullifier:?}")]
    MissingFundingInput { nullifier: NoteNullifier },
    #[error("missing signing key for the transfer input of {public_key:?}")]
    MissingSigningKey { public_key: ZkPublicKey },
    #[error("leading inscription operation does not match its transaction signing key")]
    InvalidLeadingInscriptionSigner,
    #[error(transparent)]
    Proving(#[from] KeyError),
    #[error(transparent)]
    Verification(#[from] VerificationError),
    #[error(transparent)]
    Gas(#[from] GasOverflow),
    #[error(transparent)]
    Builder(#[from] TxBuilderError),
    #[error("wallet transaction output total overflowed u64")]
    OutputTotalOverflow,
    #[error("wallet transaction inputs cannot cover its outputs")]
    FeeAccounting,
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
    #[error("Mantle transaction error: {0}")]
    SignedOpsError(#[from] lb_core::mantle::transactions::tx_list::signed_ops::Error),
}
