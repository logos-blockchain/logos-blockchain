use lb_core::{
    da::BlobId,
    mantle::{
        SignedMantleTx,
        ops::{Op, channel::blob::BlobOp},
    },
};
use thiserror::Error;

use crate::backend::TxVerifierBackend;

#[derive(Error, Debug)]
pub enum MockTxVerifierError {
    #[error("Transaction has no blob id")]
    NoBlobId,
}

pub struct MockTxVerifier;

impl TxVerifierBackend for MockTxVerifier {
    type Settings = ();
    type Tx = SignedMantleTx;
    type BlobId = BlobId;
    type Error = MockTxVerifierError;

    fn new(_settings: Self::Settings) -> Self {
        Self
    }

    fn verify(&self, _tx: &Self::Tx) -> Result<(), Self::Error> {
        Ok(())
    }

    fn blob_id(&self, tx: &Self::Tx) -> Result<Self::BlobId, Self::Error> {
        if let Some(Op::ChannelBlob(BlobOp { blob, .. })) = tx.mantle_tx.ops.first() {
            Ok(*blob)
        } else {
            Err(MockTxVerifierError::NoBlobId)
        }
    }
}
