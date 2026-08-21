use crate::mantle::{TxHash, traits::Hashable, transactions::tx_list::OpRefs};

pub trait MantleTx: Hashable<Hash = TxHash> {
    fn op_refs(&self) -> OpRefs<'_>;
}
