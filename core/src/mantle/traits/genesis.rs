use crate::mantle::{
    CryptarchiaParameter,
    ledger::verification_mode::GenesisMode,
    ops::{SignedOperation, channel::inscribe::InscriptionOp, transfer::TransferOp},
    traits::{Hashable, MantleTx},
    transactions::{GenesisDeclarations, hash::TxHash, states::Verified},
};

pub struct GenesisOps {
    pub transfer: SignedOperation<TransferOp, Verified, GenesisMode>,
    pub inscription: SignedOperation<InscriptionOp, Verified, GenesisMode>,
    pub declarations: GenesisDeclarations,
}

/// A genesis transaction as specified in the
/// [Spec](https://lip.logos.co/blockchain/raw/bedrock-genesis-block.html).
pub trait GenesisTx: Hashable<Hash = TxHash> + MantleTx {
    fn into_genesis_ops(self) -> GenesisOps;
    fn cryptarchia_parameter(&self) -> CryptarchiaParameter;
}
