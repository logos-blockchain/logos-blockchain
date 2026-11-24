use crate::{
    era::ForkDigest,
    mantle::{OpRef, TxHash, traits::Hashable, transactions::tx_list::OpRefs},
};

pub trait MantleTx: Hashable<Hash = TxHash> {
    /// The fork the transaction is bound to, or `None` if it carries no fork
    /// digest, as the transactions of the eras before fork digests do.
    fn fork_digest(&self) -> Option<ForkDigest>;
    fn op_refs(&self) -> OpRefs<'_>;
    fn op_refs_iter(&self) -> impl Iterator<Item = OpRef<'_>>;
}
