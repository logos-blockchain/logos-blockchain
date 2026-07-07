pub mod channel;
pub mod gas;
pub mod ledger;
pub mod mock;
pub mod nom;
pub mod ops;
pub mod transactions;

use std::hash::Hash;

pub use gas::{GasCalculator, GasConstants};
pub use ledger::{Note, NoteId, Utxo, Value};
pub use ops::{Op, OpProof};
use ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp};
use thiserror::Error;
pub use transactions::{CryptarchiaParameter, GenesisTime};

pub use crate::mantle::transactions::{MantleTx, SignedMantleTx, TxHash, VerificationError};
use crate::mantle::{
    gas::{Gas, GasCost, GasOverflow},
    ops::transfer::TransferOp,
    transactions::OperationVerificationHelper,
};

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
    /// Returns the bytes' that are used to form a signature of a transaction.
    ///
    /// The resulting bytes' are then used by the `HASHER`
    /// to produce the transaction's unique hash, which is what is typically
    /// signed by the transaction originator.
    fn as_signing(&self) -> Vec<u8>;
}

pub trait AuthenticatedMantleTx: Transaction<Hash = TxHash> + GasCalculator + StorageSize {
    type Context;
    /// Returns the underlying `MantleTx` that this transaction represents.
    fn mantle_tx(&self) -> &MantleTx;

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)>;

    // Gas Cost functions with context already handled
    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<GasCost, GasOverflow>;
    fn storage_gas_cost(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<GasCost, GasOverflow>;
    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<Gas, GasOverflow>;
    fn storage_gas_consumption(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<Gas, GasOverflow>;

    fn verify_ops_proofs_with_helper(
        &self,
        helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError>;
}

/// A genesis transaction as specified in
//  https://www.notion.so/nomos-tech/v1-1-Bedrock-Genesis-Block-32e261aa09df80689540ec445172b00d
pub trait GenesisTx: Transaction<Hash = TxHash> {
    fn genesis_transfer(&self) -> &TransferOp;
    fn genesis_inscription(&self) -> &InscriptionOp;
    fn cryptarchia_parameter(&self) -> CryptarchiaParameter;
    fn sdp_declarations(&self) -> impl Iterator<Item = (&SDPDeclareOp, &OpProof)>;
    fn mantle_tx(&self) -> &MantleTx;
}

impl<T: Transaction> Transaction for &T {
    const HASHER: TransactionHasher<Self> = |tx| T::HASHER(tx);
    type Hash = T::Hash;

    fn as_signing(&self) -> Vec<u8> {
        T::as_signing(self)
    }
}

impl<T: StorageSize> StorageSize for &T {
    fn storage_size(&self) -> usize {
        T::storage_size(self)
    }
}

impl<T: AuthenticatedMantleTx> AuthenticatedMantleTx for &T {
    type Context = <T as AuthenticatedMantleTx>::Context;
    fn mantle_tx(&self) -> &MantleTx {
        T::mantle_tx(self)
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)> {
        T::ops_with_proof(self)
    }

    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<GasCost, GasOverflow> {
        <T as AuthenticatedMantleTx>::total_gas_cost::<Constants>(self, context)
    }

    fn storage_gas_cost(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<GasCost, GasOverflow> {
        <T as AuthenticatedMantleTx>::storage_gas_cost(self, context)
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<Gas, GasOverflow> {
        <T as AuthenticatedMantleTx>::execution_gas_consumption::<Constants>(self, context)
    }

    fn storage_gas_consumption(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<Gas, GasOverflow> {
        <T as AuthenticatedMantleTx>::storage_gas_consumption(self, context)
    }

    fn verify_ops_proofs_with_helper(
        &self,
        operation_verification_helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError> {
        <T as AuthenticatedMantleTx>::verify_ops_proofs_with_helper(
            self,
            operation_verification_helper,
        )
    }
}

impl<T: GenesisTx> GenesisTx for &T {
    fn genesis_transfer(&self) -> &TransferOp {
        T::genesis_transfer(self)
    }
    fn genesis_inscription(&self) -> &InscriptionOp {
        T::genesis_inscription(self)
    }

    fn cryptarchia_parameter(&self) -> CryptarchiaParameter {
        T::cryptarchia_parameter(self)
    }

    fn sdp_declarations(&self) -> impl Iterator<Item = (&SDPDeclareOp, &OpProof)> {
        T::sdp_declarations(self)
    }

    fn mantle_tx(&self) -> &MantleTx {
        T::mantle_tx(self)
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid witness")]
    InvalidWitness,
}
