use lb_binary_codec::canonical::{BinaryCodec, BinaryEncode as _};
use lb_groth16::{Fr, serde::serde_fr};
use lb_mmr::MerklePath;
use lb_zktransfer::{
    ZkTransferInputsError, ZkTransferOutputNoteData, ZkTransferUnsignedInput,
    ZkTransferWitnessBuilder,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(any(test, feature = "test-utils"))]
use crate::mantle::notes::{NoteCommitment, NoteNullifier};
use crate::{
    block::BlockNumber,
    events::TxEvent,
    mantle::{
        batch::DeferredZkTransfer,
        gas::{
            Gas, GasOverflow, MainnetGasProfile, OpGasCalculator, OpGasContext, OperationGas,
            spend_gas,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, Value,
            VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{LEDGER_SET, NoteSetError, NoteSets},
        notes::{InputNullifiers, Note, OutputCommitments},
        ops::{
            OpId, SignedOperation, ZkTransferProof,
            zk_transfer::{defer_zk_transfer, preverify_zk_transfer},
        },
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct TransferOp {
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    pub excess_value: Value,
}

impl TransferOp {
    #[must_use]
    pub const fn new(
        inputs: InputNullifiers,
        outputs: OutputCommitments,
        commitment_merkle_root: Fr,
        excess_value: Value,
    ) -> Self {
        Self {
            inputs,
            outputs,
            commitment_merkle_root,
            excess_value,
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self::new(
            [NoteNullifier(Fr::from(1u64)), NoteNullifier(Fr::from(2u64))].into(),
            [
                NoteCommitment(Fr::from(3u64)),
                NoteCommitment(Fr::from(4u64)),
            ]
            .into(),
            Fr::from(5u64),
            6,
        )
    }
}

impl OpId for TransferOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TransferError {
    #[error(transparent)]
    NoteSet(#[from] NoteSetError),
    #[error(transparent)]
    ZkTransferInputs(#[from] ZkTransferInputsError),
}

pub struct TransferValidationContext<'a> {
    pub note_sets: &'a NoteSets,
    pub block_number: BlockNumber,
    pub tx_hash_view: &'a TxHashView,
}

impl ProvableOperation for TransferOp {
    type Proof = ZkTransferProof;
    const CODE: u8 = 0x00;
}

impl OperationGas<MainnetGasProfile> for TransferOp {
    const GAS_COST: Gas = Gas::new(10);
}

impl OpGasCalculator<MainnetGasProfile> for TransferOp {
    // The Operation, plus the spend of its notes.
    fn execution_gas(&self, _context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        Self::GAS_COST.checked_add(spend_gas(self.inputs.len(), self.outputs.len())?)
    }
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<TransferOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = TransferError;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();
        preverify_zk_transfer(&operation.inputs, operation.outputs.len())
    }
}

impl VerifiableOperation<StandardMode> for SignedOperation<TransferOp, Preverified, StandardMode> {
    type DeferredProof = DeferredZkTransfer;
    type Context<'a> = TransferValidationContext<'a>;
    type Error = TransferError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        // Validate the inputs against the ledger note set
        let tx_commitment_root = context
            .note_sets
            .get(LEDGER_SET)
            .expect("the ledger note set always exists")
            .spendable_tx_root(
                context.note_sets.tx_roots(),
                operation.inputs.as_slice(),
                &operation.commitment_merkle_root,
                context.block_number,
            )?;

        // Defer the proof verification so that the caller can batch it.
        Ok(defer_zk_transfer(
            self.proof(),
            &operation.inputs,
            &operation.outputs,
            operation.excess_value,
            tx_commitment_root,
            *context.tx_hash_view.as_fr(),
        )?)
    }
}

impl<Mode: VerificationMode> ExecutableOperation for SignedOperation<TransferOp, Verified, Mode> {
    type Context<'a> = NoteSets;
    type Error = TransferError;

    fn execute<'a>(
        &self,
        note_sets: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Spend the inputs and add the outputs to the ledger note set, the
        // outputs extending the buffer the next Operations of the
        // transaction are proven against.
        let note_sets = note_sets.execute_transfer(
            LEDGER_SET,
            operation.inputs.as_slice(),
            LEDGER_SET,
            operation.outputs.as_slice(),
        )?;
        Ok((note_sets, Vec::new()))
    }
}

/// The witness of a `ZkTransfer` from note openings and their paths in the
/// commitment MMR, without the secret keys of the inputs, which their holders
/// add.
///
/// # Panics
///
/// If the circuit does not fit the notes: at most 4 inputs and 8 outputs.
#[must_use]
pub fn transfer_witness(
    inputs: &[(Note, MerklePath)],
    outputs: &[Note],
    tx_commitment_root: Fr,
    msg: Fr,
) -> ZkTransferWitnessBuilder {
    inputs
        .iter()
        .try_fold(
            ZkTransferWitnessBuilder::new(tx_commitment_root, msg),
            |builder, (note, path)| {
                builder.input(ZkTransferUnsignedInput {
                    stake_public_key: *note.stake_public_key.as_fr(),
                    nonce: note.nonce,
                    value: note.value,
                    path_and_selectors: core::array::from_fn(|h| path.path_and_selectors(h).into()),
                })
            },
        )
        .and_then(|builder| {
            outputs.iter().try_fold(builder, |builder, note| {
                builder.output(ZkTransferOutputNoteData {
                    public_key: *note.public_key.as_fr(),
                    stake_public_key: *note.stake_public_key.as_fr(),
                    nonce: note.nonce,
                    value: note.value,
                })
            })
        })
        .expect("a ZkTransfer spends at most 4 notes and creates at most 8")
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use lb_groth16::Fr;
    use lb_mmr::MerklePath;

    use super::transfer_witness;
    use crate::mantle::notes::Note;

    /// Proves a `ZkTransfer` from note openings and their commitment MMR
    /// paths. Each input is a note, its secret key and its path in the
    /// commitment MMR.
    #[must_use]
    pub fn prove_transfer(
        inputs: &[(Note, Fr, MerklePath)],
        outputs: &[Note],
        tx_commitment_root: Fr,
        msg: Fr,
    ) -> lb_zktransfer::ZkTransferProof {
        let openings: Vec<(Note, MerklePath)> = inputs
            .iter()
            .map(|(note, _, path)| (*note, path.clone()))
            .collect();
        let secret_keys: Vec<Fr> = inputs
            .iter()
            .map(|(_, secret_key, _)| *secret_key)
            .collect();
        let witness = transfer_witness(&openings, outputs, tx_commitment_root, msg)
            .with_secret_keys(&secret_keys)
            .expect("one secret key per input")
            .build();
        lb_zktransfer::prove(witness.into())
            .expect("the witness proves")
            .0
    }
}

#[cfg(test)]
mod test {
    use lb_groth16::{AdditiveGroup as _, Field as _, Groth16Input};
    use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};

    use super::{test_utils::prove_transfer, *};
    use crate::mantle::{
        TxHash,
        gas::test_utils::FixedThresholds,
        note_set::{CommitmentMmr, NoteSet},
        notes::derive_note_nullifier,
        ops::op_proof::samples::SampleProof as _,
    };

    #[test]
    fn transfer_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            // 2 inputs, 2 outputs, nothing buffered: 10 + 4177 + 2 * 1758 + 2 * 58
            assert_eq!(
                TransferOp::sample().execution_gas(&FixedThresholds(threshold)),
                Ok(Gas::new(7_819))
            );
        }
    }

    fn commitment(n: u64) -> NoteCommitment {
        NoteCommitment(Fr::from(n))
    }

    fn nullifier(n: u64) -> NoteNullifier {
        NoteNullifier(Fr::from(n))
    }

    // The ledger set holds commitment(1), recorded at the end of block 0.
    fn note_sets() -> NoteSets {
        let ledger = NoteSet::empty().execute_adding(&[commitment(1)]).unwrap();
        NoteSets::new().update(LEDGER_SET, ledger).end_block(0)
    }

    fn root() -> Fr {
        note_sets().get(LEDGER_SET).unwrap().commitment_root()
    }

    fn transfer(
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
    ) -> TransferOp {
        TransferOp::new(
            InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            6,
        )
    }

    fn tx_hash_view() -> TxHashView {
        TxHashView::from(TxHash::from([9u8; 32]))
    }

    fn verify(
        operation: TransferOp,
        note_sets: &NoteSets,
    ) -> Result<DeferredZkTransfer, TransferError> {
        SignedOperation::<_, Unverified, StandardMode>::new(operation, ZkTransferProof::sample())
            .into_preverified(&())
            .expect("preverify accepts the transfer")
            .verify(&TransferValidationContext {
                note_sets,
                block_number: 1,
                tx_hash_view: &tx_hash_view(),
            })
    }

    #[test]
    fn preverify_rejects_empty_inputs() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            transfer(&[], &[commitment(2)], root()),
            ZkTransferProof::sample(),
        );
        assert_eq!(
            signed_operation.preverify(&()),
            Err(TransferError::NoteSet(NoteSetError::EmptyInputs))
        );
    }

    #[test]
    fn preverify_rejects_more_than_four_inputs() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            transfer(
                &[
                    nullifier(1),
                    nullifier(2),
                    nullifier(3),
                    nullifier(4),
                    nullifier(5),
                ],
                &[],
                root(),
            ),
            ZkTransferProof::sample(),
        );
        assert_eq!(
            signed_operation.preverify(&()),
            Err(TransferError::ZkTransferInputs(
                ZkTransferInputsError::TooManyInputs(5)
            ))
        );
    }

    #[test]
    fn preverify_rejects_more_than_eight_outputs() {
        let outputs: Vec<NoteCommitment> = (1..=9).map(commitment).collect();
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            transfer(&[nullifier(1)], &outputs, root()),
            ZkTransferProof::sample(),
        );
        assert_eq!(
            signed_operation.preverify(&()),
            Err(TransferError::ZkTransferInputs(
                ZkTransferInputsError::TooManyOutputs(9)
            ))
        );
    }

    #[test]
    fn verify_defers_the_zk_transfer_public_inputs() {
        let deferred = verify(
            transfer(&[nullifier(2)], &[commitment(3)], root()),
            &note_sets(),
        )
        .unwrap();
        let public = deferred.inputs();
        assert_eq!(
            public.inputs.map(Groth16Input::into_inner),
            [Fr::from(2u64), Fr::ZERO, Fr::ZERO, Fr::ZERO]
        );
        assert_eq!(
            public.inputs_active.map(Groth16Input::into_inner),
            [Fr::ONE, Fr::ZERO, Fr::ZERO, Fr::ZERO]
        );
        assert_eq!(public.outputs[0].into_inner(), Fr::from(3u64));
        assert_eq!(public.outputs[1].into_inner(), Fr::ZERO);
        assert_eq!(public.excess_value.into_inner(), Fr::from(6u64));
        assert_eq!(public.tx_commitment_root.into_inner(), root());
        assert_eq!(public.msg.into_inner(), *tx_hash_view().as_fr());
        assert_eq!(deferred.proof(), ZkTransferProof::sample().as_proof());
    }

    #[test]
    fn verify_extends_the_referenced_root_with_the_buffer() {
        let sets = note_sets();
        let buffered = sets
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(4)]);
        let sets = sets.update(LEDGER_SET, buffered);
        let deferred = verify(transfer(&[nullifier(2)], &[commitment(3)], root()), &sets).unwrap();
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(4))
            .unwrap()
            .frontier_root();
        assert_eq!(deferred.inputs().tx_commitment_root.into_inner(), expected);
    }

    #[test]
    fn verify_rejects_an_unknown_root() {
        assert_eq!(
            verify(
                transfer(&[nullifier(2)], &[commitment(3)], Fr::from(5u64)),
                &note_sets()
            )
            .unwrap_err(),
            TransferError::NoteSet(NoteSetError::UnknownRoot(Fr::from(5u64)))
        );
    }

    #[test]
    fn verify_rejects_a_spent_input() {
        let sets = note_sets();
        let spent = sets
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(2)])
            .unwrap();
        let sets = sets.update(LEDGER_SET, spent);
        assert_eq!(
            verify(transfer(&[nullifier(2)], &[commitment(3)], root()), &sets).unwrap_err(),
            TransferError::NoteSet(NoteSetError::AlreadySpent(nullifier(2)))
        );
    }

    #[test]
    fn execute_spends_adds_and_buffers() {
        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            transfer(&[nullifier(2)], &[commitment(3)], root()),
            ZkTransferProof::sample(),
        )
        .into_state_trusted();
        let (sets, events) = signed_operation
            .execute(note_sets())
            .expect("the input is unspent");
        let ledger = sets.get(LEDGER_SET).unwrap();
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(3))
            .unwrap()
            .frontier_root();
        assert!(ledger.contains_nullifier(&nullifier(2)));
        assert_eq!(ledger.commitment_root(), expected);
        assert_eq!(ledger.tx_commitment_root(&root(), 1), Ok(expected));
        assert_eq!(events, []);
    }

    #[test]
    fn verify_builds_the_public_inputs_a_real_proof_satisfies() {
        let secret_key = Fr::from(1u64);
        let input = Note::new(
            1_000,
            Fr::from(11u64),
            ZkKey::new(secret_key).to_public_key(),
        );
        let (mmr, path) = CommitmentMmr::new()
            .push_with_paths(input.commitment(), &mut [])
            .unwrap();
        let ledger = NoteSet::empty()
            .execute_adding(&[input.commitment()])
            .unwrap();
        let sets = NoteSets::new().update(LEDGER_SET, ledger).end_block(0);
        let root = sets.get(LEDGER_SET).unwrap().commitment_root();
        assert_eq!(root, mmr.frontier_root());
        let output = Note::new(994, Fr::from(12u64), ZkPublicKey::new(Fr::from(2u64)));
        let nullifier =
            derive_note_nullifier(&input.commitment(), &ZkKey::new(secret_key).nullifier_key());
        let proof = prove_transfer(
            &[(input, secret_key, path)],
            &[output],
            root,
            *tx_hash_view().as_fr(),
        );

        let operation = TransferOp::new([nullifier].into(), [output.commitment()].into(), root, 6);
        let deferred = SignedOperation::<_, Unverified, StandardMode>::new(
            operation,
            ZkTransferProof::new(proof),
        )
        .into_preverified(&())
        .unwrap()
        .verify(&TransferValidationContext {
            note_sets: &sets,
            block_number: 1,
            tx_hash_view: &tx_hash_view(),
        })
        .unwrap();
        assert!(lb_zktransfer::verify(deferred.proof(), deferred.inputs()).unwrap());

        let overpaid = TransferOp::new([nullifier].into(), [output.commitment()].into(), root, 7);
        let deferred = SignedOperation::<_, Unverified, StandardMode>::new(
            overpaid,
            ZkTransferProof::new(proof),
        )
        .into_preverified(&())
        .unwrap()
        .verify(&TransferValidationContext {
            note_sets: &sets,
            block_number: 1,
            tx_hash_view: &tx_hash_view(),
        })
        .unwrap();
        assert!(!lb_zktransfer::verify(deferred.proof(), deferred.inputs()).unwrap());
    }

    #[test]
    fn transfer_witness_leaves_the_secret_keys_to_their_holders() {
        let input = Note::new(1_000, Fr::from(11u64), ZkPublicKey::new(Fr::from(2u64)));
        let (mmr, path) = CommitmentMmr::new()
            .push(NoteCommitment(Fr::from(4u64)))
            .unwrap()
            .push_with_paths(input.commitment(), &mut [])
            .unwrap();
        let output = Note::new(994, Fr::from(12u64), ZkPublicKey::new(Fr::from(3u64)));
        let witness = transfer_witness(
            &[(input, path)],
            &[output],
            mmr.frontier_root(),
            Fr::from(77u64),
        );
        assert_eq!(witness.inputs_len(), 1);
        let witness = witness.with_secret_key(Fr::from(5u64)).build();
        assert_eq!(witness.wallet.inputs[0].secret_key, Fr::from(5u64));
        assert_eq!(
            witness.wallet.inputs[0].path_and_selectors[0],
            (Fr::from(4u64), true)
        );
        assert_eq!(witness.chain.inputs_active, [true, false, false, false]);
        assert_eq!(
            witness.chain.outputs_active,
            [true, false, false, false, false, false, false, false]
        );
        assert_eq!(
            (witness.chain.tx_commitment_root, witness.chain.msg),
            (mmr.frontier_root(), Fr::from(77u64))
        );
    }
}
