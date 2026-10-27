use lb_binary_codec::canonical::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Fr, serde::serde_fr};
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS, ZkTransferInputsError, ZkTransferVerifierInput};
use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "test-utils"))]
use crate::mantle::notes::{NoteCommitment, NoteNullifier};
use crate::{
    block::BlockNumber,
    events::TxEvent,
    mantle::{
        batch::DeferredZkTransfer,
        channel::{Channels, Error},
        gas::{
            Gas, GasOverflow, MainnetGasProfile, NoteSetKey, OpGasCalculator, OpGasContext,
            OperationGas, spend_gas,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{LEDGER_SET, NoteSetError, NoteSetIndex, NoteSets},
        notes::{InputNullifiers, OutputCommitments},
        ops::{
            OpId, SignedOperation, ZkTransferProof,
            channel::{ChannelId, deposit::channel_note_set},
        },
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
};

// ChannelWithdraw = ChannelId InputNullifiers CommitmentMerkleRoot
// OutputCommitments, plain field-order concat.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelWithdrawOp {
    pub channel_id: ChannelId,
    pub inputs: InputNullifiers,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    pub outputs: OutputCommitments,
}

impl OpId for ChannelWithdrawOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct WithdrawValidationContext<'a> {
    pub channels: &'a Channels,
    pub note_sets: &'a NoteSets,
    pub block_number: BlockNumber,
    pub tx_hash_view: &'a TxHashView,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WithdrawExecutionContext<'a> {
    pub channels: &'a Channels,
    pub pending_withdrawals: PendingWithdrawals,
    pub block_slot: Slot,
}

pub const WITHDRAW_DELAY: u64 = 86_400; // slots

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingWithdrawal {
    pub due: Slot,
    pub note_set: NoteSetIndex,
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
}

// Posting order is due order, since every withdrawal of a block has the same
// due slot and slots grow along a chain.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingWithdrawals(rpds::QueueSync<PendingWithdrawal>);

impl PendingWithdrawals {
    #[must_use]
    pub fn new() -> Self {
        Self(rpds::QueueSync::new_sync())
    }

    #[must_use]
    pub fn push(self, withdrawal: PendingWithdrawal) -> Self {
        Self(self.0.enqueue(withdrawal))
    }

    pub fn iter(&self) -> impl Iterator<Item = &PendingWithdrawal> {
        self.0.iter()
    }

    // Applies, in posting order, every withdrawal due at `block_slot` whose
    // inputs are unspent, drops the other due ones, keeps the rest.
    pub fn release(
        self,
        mut note_sets: NoteSets,
        block_slot: Slot,
    ) -> Result<(Self, NoteSets, Vec<PendingWithdrawal>), NoteSetError> {
        let mut queue = self.0;
        let mut released = Vec::new();
        while let Some(withdrawal) = queue.peek().filter(|w| w.due <= block_slot).cloned() {
            queue = queue.dequeue().expect("a peeked queue dequeues");
            let set = note_sets
                .get(withdrawal.note_set)
                .expect("a withdrawal's note set exists")
                .clone();
            // A step consumed one of its notes before it was due: the withdrawal is
            // dropped.
            if withdrawal
                .inputs
                .iter()
                .any(|nullifier| set.contains_nullifier(nullifier))
            {
                continue;
            }
            let set = set.execute_spending(withdrawal.inputs.as_slice())?;
            note_sets = note_sets.update(withdrawal.note_set, set);
            let ledger = note_sets
                .get(LEDGER_SET)
                .expect("the ledger note set always exists")
                .clone()
                .execute_adding(withdrawal.outputs.as_slice())?;
            note_sets = note_sets.update(LEDGER_SET, ledger);
            released.push(withdrawal);
        }
        Ok((Self(queue), note_sets, released))
    }
}

impl ProvableOperation for ChannelWithdrawOp {
    type Proof = ZkTransferProof;
    const CODE: u8 = 0x13;
}

impl OperationGas<MainnetGasProfile> for ChannelWithdrawOp {
    const GAS_COST: Gas = Gas::new(13);
}

impl OpGasCalculator<MainnetGasProfile> for ChannelWithdrawOp {
    // The Operation, plus the spend of its notes over the commitments the
    // transaction buffered in the note set of its channel before it.
    fn execution_gas(&self, context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        Self::GAS_COST.checked_add(spend_gas(
            self.inputs.len(),
            self.outputs.len(),
            context.buffered(&NoteSetKey::Channel(self.channel_id)),
        )?)
    }
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ChannelWithdrawOp, Unverified, StandardMode>
{
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

impl VerifiableOperation<StandardMode>
    for SignedOperation<ChannelWithdrawOp, Preverified, StandardMode>
{
    type DeferredProof = DeferredZkTransfer;
    type Context<'a> = WithdrawValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        // Check that the channel exists and holds a note set
        let index = channel_note_set(context.channels, operation.channel_id)?;

        // Validate the inputs against the channel note set
        let set = context
            .note_sets
            .get(index)
            .expect("a channel's note set exists");
        set.assert_spendable(
            operation.inputs.as_slice(),
            &operation.commitment_merkle_root,
            context.block_number,
        )?;
        let tx_commitment_root = set.cached_tx_commitment_root(
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
        // A withdrawal balances exactly
        let public = ZkTransferVerifierInput::new(
            &nullifiers,
            &commitments,
            0,
            tx_commitment_root,
            *context.tx_hash_view.as_fr(),
        )?;
        Ok(DeferredZkTransfer::new(*self.proof().as_proof(), public))
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<ChannelWithdrawOp, Verified, Mode>
{
    type Context<'a> = WithdrawExecutionContext<'a>;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Resolved again here since the genesis mode executes unverified
        let note_set = channel_note_set(context.channels, operation.channel_id)?;

        // The inputs are spent and the outputs created when the withdrawal is
        // released.
        let due = context.block_slot.strict_add(Slot::from(WITHDRAW_DELAY));
        context.pending_withdrawals = context.pending_withdrawals.push(PendingWithdrawal {
            due,
            note_set,
            inputs: operation.inputs.clone(),
            outputs: operation.outputs.clone(),
        });

        Ok((context, Vec::new()))
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl ChannelWithdrawOp {
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel_id: ChannelId::from([18u8; 32]),
            inputs: [NoteNullifier(Fr::from(0x13u64))].into(),
            commitment_merkle_root: Fr::from(0x29u64),
            outputs: [NoteCommitment(Fr::from(0x2au64))].into(),
        }
    }
}

#[cfg(test)]
mod test {
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
        transactions::hash::TxHash,
    };

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

    // CHANNEL owns note set 2, which holds commitment(5), recorded at the end of
    // block 0; the ledger set holds commitment(1).
    fn state() -> (Channels, NoteSets) {
        let (channels, note_sets) =
            Channels::new().create_channel(CHANNEL, Slot::from(0u64), keys(), NoteSets::new());
        let ledger = NoteSet::empty().execute_adding(&[commitment(1)]).unwrap();
        let channel = NoteSet::empty().execute_adding(&[commitment(5)]).unwrap();
        (
            channels,
            note_sets
                .update(LEDGER_SET, ledger)
                .update(2, channel)
                .end_block(0),
        )
    }

    fn channel_root() -> Fr {
        CommitmentMmr::new()
            .push(commitment(5))
            .unwrap()
            .frontier_root()
    }

    fn withdrawal(commitment_merkle_root: Fr) -> ChannelWithdrawOp {
        ChannelWithdrawOp {
            channel_id: CHANNEL,
            inputs: [nullifier(5)].into(),
            commitment_merkle_root,
            outputs: [commitment(50)].into(),
        }
    }

    fn tx_hash_view() -> TxHashView {
        TxHashView::from(TxHash::from([9u8; 32]))
    }

    fn verify(
        operation: ChannelWithdrawOp,
        channels: &Channels,
        note_sets: &NoteSets,
    ) -> Result<DeferredZkTransfer, Error> {
        SignedOperation::<_, Unverified, StandardMode>::new(operation, ZkTransferProof::sample())
            .into_preverified(&())
            .expect("preverify accepts the withdrawal")
            .verify(&WithdrawValidationContext {
                channels,
                note_sets,
                block_number: 1,
                tx_hash_view: &tx_hash_view(),
            })
    }

    #[test]
    fn preverify_rejects_empty_inputs() {
        let operation = ChannelWithdrawOp {
            inputs: InputNullifiers::default(),
            ..withdrawal(channel_root())
        };
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            operation,
            ZkTransferProof::sample(),
        );
        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::NoteSet(NoteSetError::EmptyInputs))
        );
    }

    #[test]
    fn verify_rejects_the_null_channel() {
        let (channels, note_sets) = state();
        let (channels, note_sets) =
            channels.create_channel(NULL_CHANNEL_ID, Slot::from(0u64), keys(), note_sets);
        let operation = ChannelWithdrawOp {
            channel_id: NULL_CHANNEL_ID,
            ..withdrawal(channel_root())
        };
        assert_eq!(
            verify(operation, &channels, &note_sets).unwrap_err(),
            Error::NoNoteSet {
                channel_id: NULL_CHANNEL_ID
            }
        );
    }

    #[test]
    fn verify_defers_a_balanced_zk_transfer_over_the_channel_set() {
        let (channels, note_sets) = state();
        let deferred = verify(withdrawal(channel_root()), &channels, &note_sets).unwrap();
        let public = deferred.inputs();
        assert_eq!(public.inputs[0].into_inner(), Fr::from(5u64));
        assert_eq!(public.outputs[0].into_inner(), Fr::from(50u64));
        assert_eq!(public.excess_value.into_inner(), Fr::ZERO);
        assert_eq!(public.tx_commitment_root.into_inner(), channel_root());
        assert_eq!(public.msg.into_inner(), *tx_hash_view().as_fr());
    }

    #[test]
    fn verify_rejects_a_root_of_the_ledger_set() {
        let (channels, note_sets) = state();
        let ledger_root = note_sets.get(LEDGER_SET).unwrap().commitment_root();
        assert_eq!(
            verify(withdrawal(ledger_root), &channels, &note_sets).unwrap_err(),
            Error::NoteSet(NoteSetError::UnknownRoot(ledger_root))
        );
    }

    #[test]
    fn execute_queues_the_withdrawal_until_its_due_slot() {
        let (channels, _) = state();
        let signed_operation: SignedOperation<_, _, StandardMode> =
            SignedOperation::new(withdrawal(channel_root()), ZkTransferProof::sample())
                .into_state_trusted();

        let (context, events) = signed_operation
            .execute(WithdrawExecutionContext {
                channels: &channels,
                pending_withdrawals: PendingWithdrawals::new(),
                block_slot: Slot::from(10u64),
            })
            .expect("the channel has a note set");

        assert_eq!(
            context.pending_withdrawals.iter().collect::<Vec<_>>(),
            vec![&PendingWithdrawal {
                due: Slot::from(86_410u64),
                note_set: 2,
                inputs: [nullifier(5)].into(),
                outputs: [commitment(50)].into(),
            }]
        );
        assert_eq!(events, []);
    }

    #[test]
    fn channel_withdraw_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            // 1 input, 1 output, nothing buffered: 13 + 1946 + 207 + 25
            assert_eq!(
                ChannelWithdrawOp::sample().execution_gas(&FixedThresholds(threshold)),
                Ok(Gas::new(2_191))
            );
        }
    }

    fn pending(due: u64, input: u64, output: u64) -> PendingWithdrawal {
        PendingWithdrawal {
            due: Slot::from(due),
            note_set: 2,
            inputs: [nullifier(input)].into(),
            outputs: [commitment(output)].into(),
        }
    }

    #[test]
    fn release_applies_due_withdrawals_with_unspent_inputs_only() {
        let (_, note_sets) = state();
        // A step consumed nullifier(6) before its withdrawal was due.
        let channel = note_sets
            .get(2)
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(6)])
            .unwrap();
        let note_sets = note_sets.update(2, channel);
        let ledger_before = note_sets.get(LEDGER_SET).unwrap().clone();
        let withdrawals = PendingWithdrawals::new()
            .push(pending(100, 5, 50))
            .push(pending(100, 6, 60))
            .push(pending(101, 7, 70));

        let (withdrawals, note_sets, released) =
            withdrawals.release(note_sets, Slot::from(100u64)).unwrap();

        assert_eq!(released, vec![pending(100, 5, 50)]);
        assert_eq!(
            withdrawals.iter().collect::<Vec<_>>(),
            vec![&pending(101, 7, 70)]
        );
        let channel = note_sets.get(2).unwrap();
        assert!(channel.contains_nullifier(&nullifier(5)));
        assert!(!channel.contains_nullifier(&nullifier(7)));
        let expected = ledger_before
            .clone()
            .execute_adding(&[commitment(50)])
            .unwrap();
        assert_eq!(
            note_sets.get(LEDGER_SET).unwrap().commitment_root(),
            expected.commitment_root()
        );
        // No transaction is open, so the released outputs are not buffered.
        assert_eq!(
            note_sets
                .get(LEDGER_SET)
                .unwrap()
                .tx_commitment_root(&ledger_before.commitment_root(), 1),
            Ok(ledger_before.commitment_root())
        );
    }

    #[test]
    fn release_keeps_a_withdrawal_until_its_due_slot() {
        let (_, note_sets) = state();
        let withdrawals = PendingWithdrawals::new().push(pending(100, 5, 50));
        let (withdrawals, note_sets, released) =
            withdrawals.release(note_sets, Slot::from(99u64)).unwrap();
        assert_eq!(released, []);
        assert_eq!(note_sets, state().1);
        let (withdrawals, _, released) =
            withdrawals.release(note_sets, Slot::from(100u64)).unwrap();
        assert_eq!(released, vec![pending(100, 5, 50)]);
        assert_eq!(withdrawals.iter().count(), 0);
    }

    #[test]
    fn release_drops_a_second_withdrawal_of_the_same_note() {
        let (_, note_sets) = state();
        let withdrawals = PendingWithdrawals::new()
            .push(pending(100, 5, 50))
            .push(pending(100, 5, 51));
        let (_, note_sets, released) = withdrawals.release(note_sets, Slot::from(100u64)).unwrap();
        assert_eq!(released, vec![pending(100, 5, 50)]);
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(50))
            .unwrap()
            .frontier_root();
        assert_eq!(
            note_sets.get(LEDGER_SET).unwrap().commitment_root(),
            expected
        );
    }
}
