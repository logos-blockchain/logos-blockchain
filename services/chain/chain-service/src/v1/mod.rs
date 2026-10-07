//! Blocks of version 1: the rules the chain service checks them by and applies
//! them to the ledger with.
//! [`Cryptarchia::try_apply_block_with_state_retention`] dispatches each block
//! here by its version.

mod uncle;

use lb_core::{
    block::v1,
    header::HeaderId,
    mantle::{
        gas::MainnetGasProfile,
        traits::{PreverifiedMantleTransaction, StorageSize},
    },
};
use lb_cryptarchia_engine::UncleSlots;
use lb_ledger::BatchVerifiedUpdate;

use crate::{BlockOrigin, Cryptarchia, Error};

#[expect(
    clippy::multiple_inherent_impl,
    reason = "grouping the rules of blocks of version 1 separately from the main impl"
)]
impl Cryptarchia {
    /// Checks a block of version 1 and applies it to the ledger, returning the
    /// update with the block's uncle slots. The update is not committed: the
    /// caller commits it once the consensus engine accepts the block.
    pub(crate) fn prepare_v1_block<Tx>(
        &self,
        block: v1::Block<Tx>,
        origin: BlockOrigin,
    ) -> Result<(BatchVerifiedUpdate<HeaderId>, UncleSlots), Error>
    where
        Tx: PreverifiedMantleTransaction + StorageSize + Clone,
    {
        // A block is valid only if every uncle it carries is valid.
        if origin == BlockOrigin::Network {
            self.verify_uncles(&block)?;
        }

        let header = block.header();
        let (id, parent, slot) = (header.id(), header.parent(), header.slot());
        let uncle_slots = block.uncle_headers().slots();
        let leader_proof = header.leader_proof().clone();

        let transactions = block.into_transactions();

        // Apply the block to the ledger, and batch-verify ZK proofs.
        let update = self
            .ledger
            .prepare_update::<_, _, MainnetGasProfile>(
                id,
                parent,
                slot,
                &leader_proof,
                &uncle_slots,
                transactions.into_iter(),
            )
            .map_err(|err| match err {
                lb_ledger::LedgerError::ParentNotFound(parent) => Error::ParentMissing {
                    parent,
                    info: Box::new(self.info()),
                },
                err => Error::Ledger(err),
            })?
            .verify_batch_proofs()?;
        Ok((update, uncle_slots))
    }
}
