//! Blocks of version 1: how the leader builds them. The service's main loop
//! builds a block here for each slot it wins.

mod tx_selection;

use std::pin::Pin;

use futures::{Stream, StreamExt as _, stream};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    block::{BlockTransactions, MAX_BLOCK_TRANSACTIONS_SIZE, UncleHeaders, v1},
    header::HeaderId,
    mantle::{
        OpRef, SignedOps,
        ledger::verification_mode::StandardMode,
        traits::{MantleTx, StorageSize},
        transactions::states::Preverified,
    },
    proofs::leader_proof::Groth16LeaderProof,
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_ledger::{
    LedgerState,
    config::{EraScheduledConfig, config_for_slot},
};
use lb_log_targets::diagnostic::BLEND_REACHABILITY;
use tracing::{Level, error, info, instrument};
use tx_selection::{TransactionSelection, select_transactions};

use crate::{
    Error, LOG_TARGET,
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
};

fn log_sdp_activity_selected_for_proposal<Tx>(block: &v1::Block<Tx>, ledger_state: &LedgerState)
where
    Tx: MantleTx,
{
    for (tx, active) in block.transactions_iter().flat_map(|tx| {
        tx.op_refs_iter().filter_map(move |op| match op {
            OpRef::SDPActive(active) => Some((tx, active)),
            _ => None,
        })
    }) {
        let provider_id = ledger_state
            .mantle_ledger()
            .sdp_ledger()
            .get_declaration(&active.declaration_id)
            .map(|declaration| declaration.provider_id);
        tracing::debug!(
            target: LOG_TARGET,
            diagnostic = BLEND_REACHABILITY,
            event = "sdp_activity_selected_for_proposal",
            tx_id = %tx.hash(),
            provider_id = ?provider_id,
            declaration_id = %active.declaration_id,
            proof_epoch = u32::from(active.metadata.origin_epoch()),
            proposal_block_id = %block.header().id(),
            proposal_slot = u64::from(block.header().slot()),
            "Selected SDP activity transaction for proposal"
        );
    }
}

/// Builds and signs a block of version 1 at `slot` on top of `parent`, whose
/// ledger state is `ledger_state`, with the mempool's transactions that apply
/// on it. The transactions that never apply are removed from the mempool.
#[instrument(
    target = LOG_TARGET,
    level = "debug",
    skip(
        mempool,
        ledger_state,
        ledger_eras,
        cryptarchia_api,
        proof,
        signing_key
    )
)]
#[expect(clippy::too_many_arguments, reason = "Need all args")]
pub async fn propose_block<CryptarchiaService>(
    parent: HeaderId,
    slot: Slot,
    proof: Groth16LeaderProof,
    signing_key: &Ed25519Key,
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
    mempool: &MempoolAdapter<SignedOps<Preverified, StandardMode>>,
    mut ledger_state: LedgerState,
    ledger_eras: &EraScheduledConfig,
) -> Result<v1::Block<SignedOps<Preverified, StandardMode>>, Error>
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send>,
{
    let txs_stream = mempool
        .get_mempool_view([0; 32].into())
        .await
        .map_err(Error::FetchBlockTransactions)?;

    let tx_stream: Pin<Box<_>> = Box::pin(txs_stream);

    // The chain service gathers the uncles of the era of `slot`.
    let uncle_headers = match cryptarchia_api.select_uncles(parent, slot).await {
        Ok(UncleHeaders::V1(uncle_headers)) => uncle_headers,
        Err(err) => {
            error!(target: LOG_TARGET, ?slot, %err, "failed to select uncles");
            // A proposal without uncles is still valid
            v1::UncleHeaders::empty()
        }
    };

    (ledger_state, _) = ledger_state
        .clone()
        .try_apply_header::<Groth16LeaderProof, HeaderId>(
            slot,
            &proof,
            &uncle_headers.slots(),
            ledger_eras,
        )?;
    // Collect all candidate transactions up front so the ones that fail can
    // be retried across multiple rounds.
    let TransactionSelection {
        ledger_state,
        selected_txs,
        invalid_tx_hashes,
    } = select_transactions(
        ledger_state,
        tx_stream.collect().await,
        config_for_slot(ledger_eras, slot),
    );

    if !invalid_tx_hashes.is_empty()
        && let Err(e) = mempool.remove_transactions(&invalid_tx_hashes).await
    {
        error!(target: LOG_TARGET, "Failed to remove invalid transactions from mempool: {e:?}");
    }

    let valid_tx_stream = stream::iter(selected_txs);
    let txs = txs_for_block(valid_tx_stream).await;

    let block = v1::Block::create(parent, slot, uncle_headers, proof, txs, signing_key)?;
    if tracing::enabled!(Level::DEBUG) {
        log_sdp_activity_selected_for_proposal(&block, &ledger_state);
    }

    info!(
        target: LOG_TARGET,
        "proposed block {:?} with {} transactions ({} removed)",
        block.header().id(),
        block.transactions_iter().len(),
        invalid_tx_hashes.len()
    );

    Ok(block)
}

/// Select transactions for a block, truncating the stream at the first
/// transaction that trips the block size or count limits.
async fn txs_for_block<Tx, S>(mut txs: S) -> BlockTransactions<Tx>
where
    Tx: StorageSize,
    S: Stream<Item = Tx> + Unpin,
{
    let mut block_transactions_size: usize = 0;
    let mut selected_txs = BlockTransactions::empty();

    loop {
        let Some(tx) = txs.next().await else {
            break;
        };

        let tx_size = tx.storage_size();
        let Some(next_block_transactions_size) = block_transactions_size.checked_add(tx_size)
        else {
            break;
        };

        if next_block_transactions_size > MAX_BLOCK_TRANSACTIONS_SIZE {
            break;
        }

        if selected_txs.try_push(tx).is_err() {
            break;
        }
        block_transactions_size = next_block_transactions_size;
    }

    selected_txs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestTx {
        size: usize,
    }

    impl StorageSize for TestTx {
        fn storage_size(&self) -> usize {
            self.size
        }
    }

    #[tokio::test]
    async fn block_tx_selection_respects_transaction_count_limit() {
        let txs = stream::iter(vec![
            TestTx { size: 1 };
            BlockTransactions::<TestTx>::MAX + 1
        ]);

        let selected = txs_for_block(txs).await;

        assert_eq!(selected.len(), BlockTransactions::<TestTx>::MAX);
    }

    #[tokio::test]
    async fn block_tx_selection_respects_block_size_limit() {
        let txs = stream::iter(vec![
            TestTx {
                size: MAX_BLOCK_TRANSACTIONS_SIZE / 2,
            },
            TestTx {
                size: MAX_BLOCK_TRANSACTIONS_SIZE / 2,
            },
            TestTx { size: 1 },
        ]);

        let selected = txs_for_block(txs).await;
        let selected_size: usize = selected.iter().map(StorageSize::storage_size).sum();

        assert_eq!(selected.len(), 2);
        assert_eq!(selected_size, MAX_BLOCK_TRANSACTIONS_SIZE);
    }

    #[tokio::test]
    async fn block_tx_selection_stops_at_first_transaction_that_does_not_fit() {
        // The middle transaction does not fit alongside the first, so selection
        // must stop there and must not pull the third (which would fit on its
        // own) ahead of it — doing so could drop a dependency of the third.
        let txs = stream::iter(vec![
            TestTx { size: 10 },
            TestTx {
                size: MAX_BLOCK_TRANSACTIONS_SIZE,
            },
            TestTx { size: 10 },
        ]);

        let selected = txs_for_block(txs).await;

        assert_eq!(selected.len(), 1);
        assert_eq!(selected.as_slice()[0].storage_size(), 10);
    }

    #[tokio::test]
    async fn block_tx_selection_stops_at_leading_oversized_transaction() {
        // A transaction larger than the whole block can never fit. Selection
        // stops at it rather than skipping past to later transactions, which may
        // depend on it. (In practice such transactions are filtered out before
        // reaching here, but the prefix invariant must hold regardless.)
        let txs = stream::iter(vec![
            TestTx {
                size: MAX_BLOCK_TRANSACTIONS_SIZE + 1,
            },
            TestTx { size: 1 },
        ]);

        let selected = txs_for_block(txs).await;

        assert!(selected.is_empty());
    }
}
