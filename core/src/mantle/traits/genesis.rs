use crate::mantle::{
    GenesisParameters,
    ledger::verification_mode::GenesisMode,
    notes::Note,
    ops::{SignedOperation, channel::inscribe::InscriptionOp},
    traits::{Hashable, MantleTx},
    transactions::{GenesisDeclarations, hash::TxHash, states::Verified},
};

pub struct GenesisOps {
    pub inscription: SignedOperation<InscriptionOp, Verified, GenesisMode>,
    pub distribution_inscription: SignedOperation<InscriptionOp, Verified, GenesisMode>,
    pub declarations: GenesisDeclarations,
}

/// A genesis transaction as specified in the
/// [Spec](https://lip.logos.co/blockchain/raw/bedrock-genesis-block.html).
pub trait GenesisTx: Hashable<Hash = TxHash> + MantleTx {
    fn into_genesis_ops(self) -> GenesisOps;
    fn genesis_parameters(&self) -> GenesisParameters;
    /// The initial notes, created from the distribution inscription.
    fn initial_notes(&self) -> Vec<Note>;
}
