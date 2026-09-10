use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps,
        batch::DeferredZkpVerifications,
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        traits::Hashable,
        transactions::{hash::TxHash, states::Preverified},
    },
};
use lb_ledger::{GasAndFees, LedgerState};

use crate::LOG_TARGET;

/// Progress made while selecting transactions for a block proposal.
enum AssemblyState {
    Progress,
    NoProgress,
    GasCapacityReached,
}

/// Ledger state, gas, and fees for a proposal under construction.
#[derive(Clone)]
struct BlockBuilder {
    ledger_state: LedgerState,
    gas_and_fees: GasAndFees,
}

impl BlockBuilder {
    #[must_use]
    fn new(ledger_state: LedgerState) -> Self {
        Self {
            ledger_state,
            gas_and_fees: GasAndFees::default(),
        }
    }

    fn try_add_transaction(
        self,
        tx: &SignedOps<Preverified, StandardMode>,
        ledger_config: &lb_ledger::Config,
    ) -> Result<(Self, DeferredZkpVerifications), lb_ledger::LedgerError<HeaderId>> {
        let Self {
            ledger_state,
            gas_and_fees,
        } = self;
        let (ledger_state, tx_gas_and_fees, _events, deferred_zkps) =
            ledger_state
                .try_apply_transaction::<_, HeaderId, MainnetGasProfile>(ledger_config, tx)?;
        let gas_and_fees = gas_and_fees.checked_add::<HeaderId>(tx_gas_and_fees)?;

        Ok((
            Self {
                ledger_state,
                gas_and_fees,
            },
            deferred_zkps,
        ))
    }

    #[must_use]
    fn into_ledger_state(self) -> LedgerState {
        self.ledger_state
    }
}

/// Result of selecting mempool candidates for a block proposal.
pub struct TransactionSelection {
    pub(super) ledger_state: LedgerState,
    pub(super) selected_txs: Vec<SignedOps<Preverified, StandardMode>>,
    pub(super) invalid_tx_hashes: Vec<TxHash>,
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Dependency retries and distinct invalid, proof, and block-capacity outcomes"
)]
pub fn select_transactions(
    ledger_state: LedgerState,
    mut pending: Vec<SignedOps<Preverified, StandardMode>>,
    ledger_config: &lb_ledger::Config,
) -> TransactionSelection {
    let mut block_builder = BlockBuilder::new(ledger_state);
    let mut selected_txs = Vec::new();
    let mut invalid_tx_hashes = Vec::new();

    // A transaction may only become valid once another transaction it depends
    // on has already been applied. Repeatedly attempt to apply the pending
    // transactions, retrying the full set of failures each round, while a
    // round keeps adding new transactions to the block.
    let mut assembly_state = AssemblyState::Progress;
    while matches!(assembly_state, AssemblyState::Progress) {
        assembly_state = AssemblyState::NoProgress;
        let mut still_pending = Vec::with_capacity(pending.len());

        for tx in std::mem::take(&mut pending) {
            match block_builder
                .clone()
                .try_add_transaction(&tx, ledger_config)
            {
                Ok((next_block_builder, deferred_zkps)) => match deferred_zkps.verify() {
                    Ok(()) => {
                        block_builder = next_block_builder;
                        selected_txs.push(tx);
                        assembly_state = AssemblyState::Progress;
                    }
                    Err(err) => {
                        tracing::trace!(
                            target: LOG_TARGET,
                            tx = ?tx.hash(),
                            %err,
                            "deferred ZKP verification failed during block assembly",
                        );
                        still_pending.push(tx);
                    }
                },
                Err(err @ lb_ledger::LedgerError::TooMuchExecutionGas { .. }) => {
                    tracing::trace!(
                        target: LOG_TARGET,
                        tx = ?tx.hash(),
                        %err,
                        "block execution gas limit reached during block assembly",
                    );
                    assembly_state = AssemblyState::GasCapacityReached;
                    break;
                }
                Err(err @ lb_ledger::LedgerError::TooMuchTransactionExecutionGas { .. }) => {
                    tracing::trace!(
                        target: LOG_TARGET,
                        tx = ?tx.hash(),
                        %err,
                        "transaction execution gas exceeds the block limit",
                    );
                    invalid_tx_hashes.push(tx.hash());
                }
                Err(err) => {
                    tracing::trace!(
                        target: LOG_TARGET,
                        "tx {:?} not (yet) applicable during block assembly: {:?}",
                        tx.hash(),
                        err
                    );
                    still_pending.push(tx);
                }
            }
        }

        pending = still_pending;
    }

    // Transactions that never became applicable are genuinely invalid against
    // this block's ledger state and can be evicted from the mempool. If assembly
    // stopped at the gas limit, unprocessed transactions are not invalid and
    // must remain in the mempool.
    if !matches!(assembly_state, AssemblyState::GasCapacityReached) {
        invalid_tx_hashes.extend(pending.iter().map(Hashable::hash));
    }

    TransactionSelection {
        ledger_state: block_builder.into_ledger_state(),
        selected_txs,
        invalid_tx_hashes,
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;
    use lb_core::{
        block::MAX_BLOCK_TRANSACTIONS_SIZE,
        mantle::{
            Note, Op, OpProof, Utxo,
            gas::TxGasCalculator as _,
            ledger::{Inputs, Outputs},
            ops::transfer::TransferOp,
            traits::StorageSize,
            transactions::{OpProofs, Ops, states::Unverified},
        },
    };
    use lb_key_management_system_service::keys::ZkKey;

    use super::*;
    use crate::{leadership, txs_for_block};

    fn transfer_heavy_transaction(
        transaction_index: usize,
        funding_key: &ZkKey,
        transfer_count: usize,
    ) -> (Vec<Utxo>, SignedOps<Preverified, StandardMode>) {
        let funding_utxos = (0..transfer_count)
            .map(|transfer_index| {
                let utxo_index = transaction_index * transfer_count + transfer_index;
                let mut op_id = [0; 32];
                op_id[..size_of::<usize>()].copy_from_slice(&utxo_index.to_le_bytes());
                Utxo::new(op_id, 0, Note::new(10_000_000, funding_key.to_public_key()))
            })
            .collect::<Vec<_>>();
        let ops = Ops::try_from_iter(funding_utxos.iter().map(|utxo| {
            Op::Transfer(TransferOp::new(
                Inputs::try_new(vec![utxo.id()]).unwrap(),
                Outputs::try_new(Vec::new()).unwrap(),
            ))
        }))
        .unwrap();
        let tx_hash = ops.hash();
        let signature =
            ZkKey::multi_sign(std::slice::from_ref(funding_key), &tx_hash.to_fr()).unwrap();
        let proofs = OpProofs::try_from_iter(
            std::iter::repeat_with(|| OpProof::ZkSig(signature.clone())).take(transfer_count),
        )
        .unwrap();
        let transaction = SignedOps::<Unverified, StandardMode>::from_parts(ops, proofs)
            .unwrap()
            .preverify()
            .unwrap();

        (funding_utxos, transaction)
    }

    #[tokio::test]
    async fn gas_limited_selection_returns_canonically_applicable_prefix() {
        const CANDIDATE_COUNT: usize = 22;
        const TRANSFERS_PER_TRANSACTION: usize = 255;

        let config = leadership::test_config();
        let funding_key = ZkKey::zero();
        let (funding_utxos, candidates): (Vec<_>, Vec<_>) = (0..CANDIDATE_COUNT)
            .map(|transaction_index| {
                transfer_heavy_transaction(
                    transaction_index,
                    &funding_key,
                    TRANSFERS_PER_TRANSACTION,
                )
            })
            .unzip();

        let ledger_state = LedgerState::from_utxos(funding_utxos.into_iter().flatten(), &config);
        let gas_context = ledger_state.tx_context().gas_context;
        let individual_gas = candidates[0]
            .op_refs()
            .execution_gas_consumption::<MainnetGasProfile>(&gas_context)
            .unwrap();
        assert!(candidates.iter().all(|candidate| {
            candidate
                .op_refs()
                .execution_gas_consumption::<MainnetGasProfile>(&gas_context)
                .unwrap()
                == individual_gas
        }));
        assert!(
            candidates
                .iter()
                .map(StorageSize::storage_size)
                .sum::<usize>()
                <= MAX_BLOCK_TRANSACTIONS_SIZE
        );
        let all_candidates_result = ledger_state
            .clone()
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                candidates.iter().cloned(),
            );
        let all_candidates_error = all_candidates_result.err();
        assert!(
            matches!(
                &all_candidates_error,
                Some(lb_ledger::LedgerError::TooMuchExecutionGas { .. })
            ),
            "individual gas: {individual_gas:?}, error: {all_candidates_error:?}"
        );

        let selection = select_transactions(ledger_state.clone(), candidates, &config);
        assert_eq!(selection.selected_txs.len(), CANDIDATE_COUNT - 1);
        assert!(selection.invalid_tx_hashes.is_empty());

        let block_txs = txs_for_block(stream::iter(selection.selected_txs)).await;
        assert_eq!(block_txs.len(), CANDIDATE_COUNT - 1);
        ledger_state
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                block_txs.into_iter(),
            )
            .expect("the selected prefix must pass canonical application");
    }
}
