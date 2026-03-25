use std::{hash::Hash, pin::Pin};

use futures::Stream;
use thiserror::Error;

pub mod encoding;
pub mod gas;
pub mod genesis_tx;
pub mod ledger;
#[cfg(feature = "mock")]
pub mod mock;
pub mod ops;
pub mod select;
pub mod tx;
pub mod tx_builder;

pub use gas::{GasConstants, GasCost};
use lb_groth16::Fr;
pub use ledger::{Note, NoteId, Utxo, Value};
pub use ops::{Op, OpProof};
use ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp};
pub use tx::{MantleTx, SignedMantleTx, TxHash};

use crate::mantle::ops::transfer::TransferOp;

pub const MAX_MANTLE_TXS: usize = 1024;

pub type TransactionHasher<T> = fn(&T) -> <T as Transaction>::Hash;

pub trait StorageSize {
    fn storage_size(&self) -> usize;
}

pub trait Transaction {
    const HASHER: TransactionHasher<Self>;
    type Hash: Hash + Eq + Clone;
    fn hash(&self) -> Self::Hash {
        Self::HASHER(self)
    }
    /// Returns the Fr's that are used to form a signature of a transaction.
    ///
    /// The resulting Fr's are then used by the `HASHER`
    /// to produce the transaction's unique hash, which is what is typically
    /// signed by the transaction originator.
    fn as_signing_frs(&self) -> Vec<Fr>;
}

pub trait AuthenticatedMantleTx: Transaction<Hash = TxHash> + GasCost + StorageSize {
    /// Returns the underlying `MantleTx` that this transaction represents.
    fn mantle_tx(&self) -> &MantleTx;

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)>;
}

/// A genesis transaction as specified in
//  https://www.notion.so/nomos-tech/v1-1-Bedrock-Genesis-Block-32e261aa09df80689540ec445172b00d
pub trait GenesisTx: Transaction<Hash = TxHash> {
    fn genesis_transfer(&self) -> &TransferOp;
    fn genesis_inscription(&self) -> &InscriptionOp;
    fn sdp_declarations(&self) -> impl Iterator<Item = (&SDPDeclareOp, &OpProof)>;
    fn mantle_tx(&self) -> &MantleTx;
}

impl<T: Transaction> Transaction for &T {
    const HASHER: TransactionHasher<Self> = |tx| T::HASHER(tx);
    type Hash = T::Hash;

    fn as_signing_frs(&self) -> Vec<Fr> {
        T::as_signing_frs(self)
    }
}

impl<T: StorageSize> StorageSize for &T {
    fn storage_size(&self) -> usize {
        T::storage_size(self)
    }
}

impl<T: AuthenticatedMantleTx> AuthenticatedMantleTx for &T {
    fn mantle_tx(&self) -> &MantleTx {
        T::mantle_tx(self)
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)> {
        T::ops_with_proof(self)
    }
}

impl<T: GenesisTx> GenesisTx for &T {
    fn genesis_transfer(&self) -> &TransferOp {
        T::genesis_transfer(self)
    }
    fn genesis_inscription(&self) -> &InscriptionOp {
        T::genesis_inscription(self)
    }

    fn sdp_declarations(&self) -> impl Iterator<Item = (&SDPDeclareOp, &OpProof)> {
        T::sdp_declarations(self)
    }

    fn mantle_tx(&self) -> &MantleTx {
        T::mantle_tx(self)
    }
}

pub trait TxSelect {
    type Tx: Transaction;
    type Settings: Clone;
    fn new(settings: Self::Settings) -> Self;

    fn select_tx_from<'i, S>(&self, txs: S) -> Pin<Box<dyn Stream<Item = Self::Tx> + Send + 'i>>
    where
        S: Stream<Item = Self::Tx> + Send + 'i;
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid witness")]
    InvalidWitness,
}
