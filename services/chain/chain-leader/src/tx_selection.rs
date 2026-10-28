use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps,
        batch::DeferredProofs,
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
    ) -> Result<(Self, DeferredProofs), lb_ledger::LedgerError<HeaderId>> {
        let Self {
            ledger_state,
            gas_and_fees,
        } = self;
        let (ledger_state, tx_gas_and_fees, _events, deferred_proofs) =
            ledger_state
                .try_apply_transaction::<_, HeaderId, MainnetGasProfile>(ledger_config, tx)?;
        let gas_and_fees = gas_and_fees.checked_add::<HeaderId>(tx_gas_and_fees)?;

        Ok((
            Self {
                ledger_state,
                gas_and_fees,
            },
            deferred_proofs,
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
                Ok((next_block_builder, deferred_proofs)) => match deferred_proofs.verify() {
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
                            "deferred proof verification failed during block assembly",
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
            Note, Op, OpProof,
            gas::TxGasCalculator as _,
            note_set::{CommitmentMmr, LEDGER_SET},
            notes::{OutputCommitments, derive_note_nullifier},
            ops::{
                NoOpProof, ZkTransferProof,
                program::ProgramRegisterOp,
                transfer::{TransferOp, prove_transfer},
            },
            programs::ProgramVerificationKey,
            traits::StorageSize,
            transactions::{OpProofs, Ops, states::Unverified},
        },
    };
    use lb_groth16::{Fr, test_utils::three_input_setup};
    use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};

    use super::*;
    use crate::{leadership, txs_for_block};

    fn funding_notes(funding_key: &ZkKey, count: usize) -> Vec<Note> {
        (0..count)
            .map(|note_index| {
                Note::new(
                    10_000_000_000_000_000,
                    Fr::from(note_index as u64),
                    funding_key.to_public_key(),
                )
            })
            .collect()
    }

    // Program registrations per transaction: each one costs 6843 gas plus its
    // bytes, so that six transactions fit in the block Execution Gas limit of
    // 3_173_548 and a seventh does not.
    const PROGRAM_REGISTRATIONS_PER_TX: usize = 70;

    // Each transaction pays its fees with one Transfer and fills its execution
    // gas with Program registrations, which carry no proof, so only one
    // ZkTransfer is proven per transaction.
    fn gas_heavy_transactions(
        funding_notes: &[Note],
        funding_key: &ZkKey,
        commitment_root: Fr,
    ) -> Vec<SignedOps<Preverified, StandardMode>> {
        let verification_key = ProgramVerificationKey::try_from(&three_input_setup(1).0).unwrap();
        let mut mmr = CommitmentMmr::new();
        let mut paths = Vec::new();
        for note in funding_notes {
            let (next, path) = mmr.push_with_paths(note.commitment(), &mut paths).unwrap();
            mmr = next;
            paths.push(path);
        }
        assert_eq!(mmr.frontier_root(), commitment_root);

        funding_notes
            .iter()
            .zip(paths)
            .map(|(note, path)| {
                let ops = Ops::try_from_iter(
                    std::iter::once(Op::Transfer(TransferOp::new(
                        [derive_note_nullifier(
                            &note.commitment(),
                            funding_key.as_fr(),
                        )]
                        .into(),
                        OutputCommitments::default(),
                        commitment_root,
                        note.value,
                    )))
                    .chain(std::iter::repeat_n(
                        Op::ProgramRegister(ProgramRegisterOp {
                            verification_key: verification_key.clone(),
                        }),
                        PROGRAM_REGISTRATIONS_PER_TX,
                    )),
                )
                .unwrap();
                let transfer_proof = prove_transfer(
                    &[(*note, *funding_key.as_fr(), path)],
                    &[],
                    commitment_root,
                    ops.hash().to_fr(),
                );
                let proofs = OpProofs::try_from_iter(
                    std::iter::once(OpProof::ZkTransfer(ZkTransferProof::new(transfer_proof)))
                        .chain(std::iter::repeat_n(
                            OpProof::None(NoOpProof),
                            PROGRAM_REGISTRATIONS_PER_TX,
                        )),
                )
                .unwrap();
                SignedOps::<Unverified, StandardMode>::from_parts(ops, proofs)
                    .unwrap()
                    .preverify()
                    .unwrap()
            })
            .collect()
    }

    #[tokio::test]
    async fn gas_limited_selection_returns_canonically_applicable_prefix() {
        const CANDIDATE_COUNT: usize = 7;

        let config = leadership::test_config();
        let funding_key = ZkKey::zero();
        let funding_notes = funding_notes(&funding_key, CANDIDATE_COUNT);
        let ledger_state = LedgerState::from_notes(funding_notes.iter().copied(), &config);
        let commitment_root = ledger_state
            .latest_note_sets()
            .get(LEDGER_SET)
            .expect("the ledger set exists")
            .commitment_root();
        let candidates = gas_heavy_transactions(&funding_notes, &funding_key, commitment_root);
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
                ZkPublicKey::zero(),
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
        assert_eq!(selection.invalid_tx_hashes, []);

        let block_txs = txs_for_block(stream::iter(selection.selected_txs)).await;
        assert_eq!(block_txs.len(), CANDIDATE_COUNT - 1);
        let _apply_result = ledger_state
            .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
                &config,
                ZkPublicKey::zero(),
                block_txs.into_iter(),
            )
            .expect("the selected prefix must pass canonical application");
    }
}
