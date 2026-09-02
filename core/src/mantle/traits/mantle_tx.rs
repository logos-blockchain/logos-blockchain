use crate::mantle::{OpRef, TxHash, traits::Hashable, transactions::tx_list::OpRefs};

pub trait MantleTx: Hashable<Hash = TxHash> {
    fn op_refs(&self) -> OpRefs<'_>;
    fn op_refs_iter(&self) -> impl Iterator<Item = OpRef<'_>>;
}
