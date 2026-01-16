use std::{hash::Hash, pin::Pin};

use futures::Stream;
use key_management_system_keys::keys::ZkSignature;
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
use groth16::Fr;
pub use ledger::{Note, NoteId, Utxo, Value};
pub use ops::{Op, OpProof};
use ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp};
pub use tx::{MantleTx, SignedMantleTx, TxHash};

pub const MAX_MANTLE_TXS: usize = 1024;

pub type TransactionHasher<T> = fn(&T) -> <T as Transaction>::Hash;

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

pub trait AuthenticatedMantleTx: Transaction<Hash = TxHash> + GasCost {
    /// Returns the underlying `MantleTx` that this transaction represents.
    fn mantle_tx(&self) -> &MantleTx;

    /// Returns the proof of the ledger transaction
    fn ledger_tx_proof(&self) -> &ZkSignature;

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)>;
}

/// A genesis transaction as specified in
//  https://www.notion.so/nomos-tech/Bedrock-Genesis-Block-21d261aa09df80bb8dc3c768802eb527?d=27a261aa09df808e9c66001cf0585dee
pub trait GenesisTx: Transaction<Hash = TxHash> {
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

impl<T: AuthenticatedMantleTx> AuthenticatedMantleTx for &T {
    fn mantle_tx(&self) -> &MantleTx {
        T::mantle_tx(self)
    }

    fn ledger_tx_proof(&self) -> &ZkSignature {
        T::ledger_tx_proof(self)
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)> {
        T::ops_with_proof(self)
    }
}

impl<T: GenesisTx> GenesisTx for &T {
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
