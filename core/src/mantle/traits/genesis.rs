use crate::mantle::{
    CryptarchiaParameter, OpProof,
    ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp, transfer::TransferOp},
    traits::Hashable,
    transactions::{hash::TxHash, mantle_tx::RawMantleTx},
};

/// A genesis transaction as specified in the
/// [Spec](https://lip.logos.co/blockchain/raw/bedrock-genesis-block.html).
pub trait GenesisTx: Hashable<Hash = TxHash> {
    fn genesis_transfer(&self) -> &TransferOp;
    fn genesis_inscription(&self) -> &InscriptionOp;
    fn cryptarchia_parameter(&self) -> CryptarchiaParameter;
    fn sdp_declarations(&self) -> impl Iterator<Item = (&SDPDeclareOp, &OpProof)>;
    fn mantle_tx(&self) -> &RawMantleTx;
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

    fn mantle_tx(&self) -> &RawMantleTx {
        T::mantle_tx(self)
    }
}
