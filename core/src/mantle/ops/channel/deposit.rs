use lb_binary_codec::canonical::{BinaryCodec, BinaryEncode as _};
use lb_groth16::{Fr, serde::serde_fr};
use lb_utils::bounded::UpperBoundedVec;
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS, ZkTransferInputsError, ZkTransferVerifierInput};
use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "test-utils"))]
use crate::mantle::notes::{NoteCommitment, NoteNullifier};
use crate::{
    block::BlockNumber,
    events::{TxEvent, TxEventPayload},
    mantle::{
        batch::DeferredZkTransfer,
        channel::{Channels, Error},
        gas::{
            Gas, GasOverflow, MainnetGasProfile, NoteSetKey, OpGasCalculator, OpGasContext,
            OperationGas, spend_gas,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, Value,
            VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{LEDGER_SET, NoteSetError, NoteSetIndex, NoteSets},
        notes::{InputNullifiers, OutputCommitments},
        ops::{OpId, SignedOperation, ZkTransferProof, channel::ChannelId},
        transactions::{
            hash::{TxHash, TxHashView},
            states::{Preverified, Unverified, Verified},
        },
    },
};

pub const MAX_METADATA_SIZE: usize = u32::MAX as usize;
pub type Metadata = UpperBoundedVec<u8, { MAX_METADATA_SIZE }>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct DepositOp {
    pub channel_id: ChannelId,
    pub inputs: InputNullifiers,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    pub outputs: OutputCommitments,
    pub excess_value: Value,
    pub metadata: Metadata,
}

impl DepositOp {
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel_id: ChannelId::from([16u8; 32]),
            inputs: [NoteNullifier(Fr::from(0x11u64))].into(),
            commitment_merkle_root: Fr::from(0x26u64),
            outputs: [NoteCommitment(Fr::from(0x27u64))].into(),
            excess_value: 0x28,
            metadata: Metadata::try_from(b"deposit-metadata".to_vec())
                .expect("Metadata is within bounds."),
        }
    }
}

impl OpId for DepositOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct DepositValidationContext<'a> {
    pub channels: &'a Channels,
    pub note_sets: &'a NoteSets,
    pub block_number: BlockNumber,
    pub tx_hash_view: &'a TxHashView,
}

pub struct DepositExecutionContext<'a> {
    pub channels: &'a Channels,
    pub note_sets: NoteSets,
    pub tx_hash: TxHash,
}

// The index of the note set the channel holds its notes in.
pub(super) fn channel_note_set(
    channels: &Channels,
    channel_id: ChannelId,
) -> Result<NoteSetIndex, Error> {
    channels
        .channel_state(&channel_id)
        .ok_or(Error::ChannelNotFound { channel_id })?
        .note_set
        .ok_or(Error::NoNoteSet { channel_id })
}

impl ProvableOperation for DepositOp {
    type Proof = ZkTransferProof;
    const CODE: u8 = 0x12;
}

impl OperationGas<MainnetGasProfile> for DepositOp {
    const GAS_COST: Gas = Gas::new(16);
}

impl OpGasCalculator<MainnetGasProfile> for DepositOp {
    // The Operation, plus the spend of its notes over the commitments the
    // transaction buffered in the ledger note set before it.
    fn execution_gas(&self, context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        Self::GAS_COST.checked_add(spend_gas(
            self.inputs.len(),
            self.outputs.len(),
            context.buffered(&NoteSetKey::Ledger),
        )?)
    }
}

impl PreverifiableOperation<StandardMode> for SignedOperation<DepositOp, Unverified, StandardMode> {
    type Context<'a> = ();
    type Error = Error;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Ensure the inputs is non-empty
        if operation.inputs.is_empty() {
            return Err(NoteSetError::EmptyInputs.into());
        }

        // Ensure the ZkTransfer circuit fits the inputs and the outputs
        if operation.inputs.len() > MAX_INPUTS {
            return Err(ZkTransferInputsError::TooManyInputs(operation.inputs.len()).into());
        }
        if operation.outputs.len() > MAX_OUTPUTS {
            return Err(ZkTransferInputsError::TooManyOutputs(operation.outputs.len()).into());
        }

        Ok(())
    }
}

impl VerifiableOperation<StandardMode> for SignedOperation<DepositOp, Preverified, StandardMode> {
    type DeferredProof = DeferredZkTransfer;
    type Context<'a> = DepositValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        // Check that the channel exists and holds a note set
        channel_note_set(context.channels, operation.channel_id)?;

        // Validate the inputs against the ledger note set
        let ledger = context
            .note_sets
            .get(LEDGER_SET)
            .expect("the ledger note set always exists");
        ledger.assert_spendable(
            operation.inputs.as_slice(),
            &operation.commitment_merkle_root,
            context.block_number,
        )?;
        let tx_commitment_root = ledger.cached_tx_commitment_root(
            context.note_sets.tx_roots(),
            &operation.commitment_merkle_root,
            context.block_number,
        )?;

        // Defer the proof verification so that the caller can batch it.
        let nullifiers: Vec<Fr> = operation
            .inputs
            .iter()
            .map(|nullifier| nullifier.0)
            .collect();
        let commitments: Vec<Fr> = operation
            .outputs
            .iter()
            .map(|commitment| commitment.0)
            .collect();
        let public = ZkTransferVerifierInput::new(
            &nullifiers,
            &commitments,
            operation.excess_value,
            tx_commitment_root,
            *context.tx_hash_view.as_fr(),
        )?;
        Ok(DeferredZkTransfer::new(*self.proof().as_proof(), public))
    }
}

impl<Mode: VerificationMode> ExecutableOperation for SignedOperation<DepositOp, Verified, Mode> {
    type Context<'a> = DepositExecutionContext<'a>;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Resolved again here since the genesis mode executes unverified
        let channel_set = channel_note_set(context.channels, operation.channel_id)?;

        // Spend the inputs in the ledger note set
        let ledger = context
            .note_sets
            .get(LEDGER_SET)
            .expect("the ledger note set always exists")
            .clone()
            .execute_spending(operation.inputs.as_slice())?;

        // Add the outputs to the channel note set
        let channel = context
            .note_sets
            .get(channel_set)
            .expect("the note set of a channel exists")
            .clone()
            .execute_adding(operation.outputs.as_slice())?
            // Step 5 of the Validate of CHANNEL_DEPOSIT, done at execution as
            // for every Operation.
            .buffer_outputs(operation.outputs.as_slice());
        context.note_sets = context
            .note_sets
            .update(LEDGER_SET, ledger)
            .update(channel_set, channel);

        let events = vec![TxEvent::new(
            context.tx_hash,
            operation.op_id(),
            TxEventPayload::Deposit {
                channel_id: operation.channel_id,
                metadata: operation.metadata.clone(),
                outputs: operation.outputs.clone(),
            },
        )];

        Ok((context, events))
    }
}

#[cfg(test)]
mod test {
    use lb_cryptarchia_engine::Slot;
    use lb_groth16::AdditiveGroup as _;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;
    use crate::mantle::{
        gas::test_utils::FixedThresholds,
        note_set::{CommitmentMmr, NoteSet},
        ops::{
            channel::{NULL_CHANNEL_ID, UnverifiedChannelKeys},
            op_proof::samples::SampleProof as _,
        },
    };

    #[test]
    fn deposit_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            // 1 input, 1 output, nothing buffered: 16 + 1773 + 838 + 25
            assert_eq!(
                DepositOp::sample().execution_gas(&FixedThresholds(threshold)),
                Ok(Gas::new(2_652))
            );
        }
    }

    const CHANNEL: ChannelId = ChannelId([1u8; 32]);

    fn commitment(n: u64) -> NoteCommitment {
        NoteCommitment(Fr::from(n))
    }

    fn nullifier(n: u64) -> NoteNullifier {
        NoteNullifier(Fr::from(n))
    }

    fn keys() -> UnverifiedChannelKeys {
        UnverifiedChannelKeys::from(
            Ed25519Key::from_bytes(&[15; 32])
                .public_key()
                .into_unverified(),
        )
    }

    // CHANNEL owns note set 2; the ledger set holds commitment(1), recorded at the
    // end of block 0.
    fn state() -> (Channels, NoteSets) {
        let (channels, note_sets) =
            Channels::new().create_channel(CHANNEL, Slot::from(0u64), keys(), NoteSets::new());
        let ledger = NoteSet::empty().execute_adding(&[commitment(1)]).unwrap();
        (channels, note_sets.update(LEDGER_SET, ledger).end_block(0))
    }

    fn root() -> Fr {
        state().1.get(LEDGER_SET).unwrap().commitment_root()
    }

    fn deposit(
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
    ) -> DepositOp {
        DepositOp {
            channel_id: CHANNEL,
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            excess_value: 6,
            metadata: Metadata::empty(),
        }
    }

    fn tx_hash_view() -> TxHashView {
        TxHashView::from(TxHash::from([9u8; 32]))
    }

    fn verify(
        operation: DepositOp,
        channels: &Channels,
        note_sets: &NoteSets,
    ) -> Result<DeferredZkTransfer, Error> {
        SignedOperation::<_, Unverified, StandardMode>::new(operation, ZkTransferProof::sample())
            .into_preverified(&())
            .expect("preverify accepts the deposit")
            .verify(&DepositValidationContext {
                channels,
                note_sets,
                block_number: 1,
                tx_hash_view: &tx_hash_view(),
            })
    }

    #[test]
    fn preverify_rejects_empty_inputs() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            deposit(&[], &[commitment(2)], root()),
            ZkTransferProof::sample(),
        );
        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::NoteSet(NoteSetError::EmptyInputs))
        );
    }

    #[test]
    fn preverify_rejects_more_than_four_inputs() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            deposit(
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
            Err(Error::ZkTransferInputs(
                ZkTransferInputsError::TooManyInputs(5)
            ))
        );
    }

    #[test]
    fn verify_rejects_an_unknown_channel() {
        let (_, note_sets) = state();
        assert_eq!(
            verify(
                deposit(&[nullifier(2)], &[commitment(3)], root()),
                &Channels::new(),
                &note_sets
            )
            .unwrap_err(),
            Error::ChannelNotFound {
                channel_id: CHANNEL
            }
        );
    }

    #[test]
    fn verify_rejects_the_null_channel() {
        let (channels, note_sets) = state();
        let (channels, note_sets) =
            channels.create_channel(NULL_CHANNEL_ID, Slot::from(0u64), keys(), note_sets);
        let operation = DepositOp {
            channel_id: NULL_CHANNEL_ID,
            ..deposit(&[nullifier(2)], &[commitment(3)], root())
        };
        assert_eq!(
            verify(operation, &channels, &note_sets).unwrap_err(),
            Error::NoNoteSet {
                channel_id: NULL_CHANNEL_ID
            }
        );
    }

    #[test]
    fn verify_defers_the_zk_transfer_over_the_ledger_set() {
        let (channels, note_sets) = state();
        let deferred = verify(
            deposit(&[nullifier(2)], &[commitment(3)], root()),
            &channels,
            &note_sets,
        )
        .unwrap();
        let public = deferred.inputs();
        assert_eq!(public.inputs[0].into_inner(), Fr::from(2u64));
        assert_eq!(public.inputs[1].into_inner(), Fr::ZERO);
        assert_eq!(public.outputs[0].into_inner(), Fr::from(3u64));
        assert_eq!(public.excess_value.into_inner(), Fr::from(6u64));
        assert_eq!(public.tx_commitment_root.into_inner(), root());
        assert_eq!(public.msg.into_inner(), *tx_hash_view().as_fr());
    }

    #[test]
    fn verify_extends_the_ledger_root_with_the_ledger_buffer_only() {
        let (channels, note_sets) = state();
        let ledger = note_sets
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(4)]);
        let channel = note_sets
            .get(2)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(9)]);
        let note_sets = note_sets.update(LEDGER_SET, ledger).update(2, channel);
        let deferred = verify(
            deposit(&[nullifier(2)], &[commitment(3)], root()),
            &channels,
            &note_sets,
        )
        .unwrap();
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(4))
            .unwrap()
            .frontier_root();
        assert_eq!(deferred.inputs().tx_commitment_root.into_inner(), expected);
    }

    #[test]
    fn verify_rejects_a_spent_ledger_input() {
        let (channels, note_sets) = state();
        let spent = note_sets
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(2)])
            .unwrap();
        let note_sets = note_sets.update(LEDGER_SET, spent);
        assert_eq!(
            verify(
                deposit(&[nullifier(2)], &[commitment(3)], root()),
                &channels,
                &note_sets
            )
            .unwrap_err(),
            Error::NoteSet(NoteSetError::AlreadySpent(nullifier(2)))
        );
    }

    #[test]
    fn execute_spends_ledger_inputs_and_adds_the_outputs_to_the_channel_set_and_its_buffer() {
        let (channels, note_sets) = state();
        let operation = deposit(&[nullifier(2)], &[commitment(3)], root());
        let signed_operation: SignedOperation<_, _, StandardMode> =
            SignedOperation::new(operation.clone(), ZkTransferProof::sample()).into_state_trusted();
        let tx_hash = TxHash::from([9u8; 32]);

        let (context, events) = signed_operation
            .execute(DepositExecutionContext {
                channels: &channels,
                note_sets,
                tx_hash,
            })
            .expect("the input is unspent");

        let ledger = context.note_sets.get(LEDGER_SET).unwrap();
        assert!(ledger.contains_nullifier(&nullifier(2)));
        assert_eq!(ledger.commitment_root(), root());
        let channel = context.note_sets.get(2).unwrap();
        let expected = CommitmentMmr::new()
            .push(commitment(3))
            .unwrap()
            .frontier_root();
        assert_eq!(channel.commitment_root(), expected);
        assert_eq!(
            channel.tx_commitment_root(&CommitmentMmr::new().frontier_root(), 1),
            Ok(expected)
        );
        assert_eq!(
            events,
            vec![TxEvent::new(
                tx_hash,
                operation.op_id(),
                TxEventPayload::Deposit {
                    channel_id: CHANNEL,
                    metadata: Metadata::empty(),
                    outputs: operation.outputs,
                },
            )]
        );
    }
}
