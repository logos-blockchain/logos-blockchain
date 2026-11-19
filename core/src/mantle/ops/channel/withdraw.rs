use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Fr, serde::serde_fr};
use rpds::{RedBlackTreeMapSync, RedBlackTreeSetSync};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(any(test, feature = "test-utils"))]
use crate::mantle::notes::{NoteCommitment, NoteNullifier};
use crate::{
    block::BlockNumber,
    events::TxEvent,
    mantle::{
        batch::DeferredZkTransfer,
        channel::{Channels, Error},
        gas::{Gas, GasOverflow, GasProfile, OpGasCalculator, OpGasContext},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{NoteSetError, NoteSetIndex, NoteSets, TxNoteState},
        notes::{InputNullifiers, OutputCommitments},
        ops::{
            OpId, SignedOperation, ZkTransferProof,
            channel::ChannelId,
            zk_transfer::{defer_zk_transfer, preverify_zk_transfer},
        },
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
};

// ChannelWithdraw = ChannelId InputNullifiers OutputCommitments
// CommitmentMerkleRoot, plain field-order concat.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryEncode, BinaryDecode)]
pub struct ChannelWithdrawOp {
    pub channel_id: ChannelId,
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
}

impl OpId for ChannelWithdrawOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct WithdrawValidationContext<'a> {
    pub channels: &'a Channels,
    pub note_sets: &'a NoteSets,
    pub tx_notes: &'a TxNoteState,
    pub block_number: BlockNumber,
    pub tx_hash_view: &'a TxHashView,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WithdrawExecutionContext<'a> {
    pub channels: &'a Channels,
    pub pending_withdrawals: PendingWithdrawals,
    pub block_slot: Slot,
    /// The withdrawal delay of the era of `block_slot`.
    pub withdraw_delay: Slot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingWithdrawal {
    pub due: Slot,
    pub note_set_index: NoteSetIndex,
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
}

// The channel and SDP withdrawals waiting for their due slot, in posting order.
// An SDP withdrawal is due at the start of an epoch and a channel withdrawal
// the withdrawal delay of its era after its posting, so posting order is not
// due order: the withdrawals are also indexed by due slot, so that a block
// reads the ones due at its slot, not every pending one.
#[derive(Clone, Debug, Default)]
pub struct PendingWithdrawals {
    // The withdrawals by their position in posting order.
    by_posting: RedBlackTreeMapSync<u64, PendingWithdrawal>,
    // The due slot and the posting position of each withdrawal.
    by_due: RedBlackTreeSetSync<(Slot, u64)>,
    // The posting position of the next withdrawal.
    next: u64,
}

// The withdrawals in posting order, whatever positions they were posted at.
impl PartialEq for PendingWithdrawals {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Eq for PendingWithdrawals {}

// A sequence of the withdrawals in posting order.
impl Serialize for PendingWithdrawals {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for PendingWithdrawals {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<PendingWithdrawal>::deserialize(deserializer)?
            .into_iter()
            .fold(Self::new(), Self::push))
    }
}

impl PendingWithdrawals {
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_posting: RedBlackTreeMapSync::new_sync(),
            by_due: RedBlackTreeSetSync::new_sync(),
            next: 0,
        }
    }

    #[must_use]
    pub fn push(mut self, withdrawal: PendingWithdrawal) -> Self {
        let position = self.next;
        self.next += 1;
        self.by_due.insert_mut((withdrawal.due, position));
        self.by_posting.insert_mut(position, withdrawal);
        self
    }

    pub fn iter(&self) -> impl Iterator<Item = &PendingWithdrawal> {
        self.by_posting.values()
    }

    fn due(&self, block_slot: Slot) -> impl Iterator<Item = (Slot, u64)> + '_ {
        self.by_due.range(..=(block_slot, u64::MAX)).copied()
    }

    // Applies, in posting order, every withdrawal due at `block_slot` whose
    // inputs are unspent, drops the other due ones, keeps the rest. An SDP
    // withdrawal has no input, so it is always applied.
    pub fn release(
        mut self,
        mut note_sets: NoteSets,
        block_slot: Slot,
    ) -> Result<(Self, NoteSets, Vec<PendingWithdrawal>), NoteSetError> {
        let mut due = self.due(block_slot).collect::<Vec<_>>();
        due.sort_unstable_by_key(|&(_, position)| position);

        let mut released = Vec::new();
        for (due_slot, position) in due {
            let withdrawal = self
                .by_posting
                .get(&position)
                .expect("an indexed withdrawal is pending")
                .clone();
            self.by_posting.remove_mut(&position);
            self.by_due.remove_mut(&(due_slot, position));

            let set = note_sets
                .get(withdrawal.note_set_index)
                .expect("a withdrawal's note set exists")
                .clone();
            // A step consumed one of its notes before it was due: the
            // withdrawal is dropped.
            if set.contains_any_nullifier(&withdrawal.inputs)? {
                continue;
            }

            let set = set.execute_spending(withdrawal.inputs.as_slice())?;
            note_sets = note_sets.update(withdrawal.note_set_index, set);
            let ledger = note_sets
                .ledger()
                .clone()
                .execute_adding(withdrawal.outputs.as_slice())?;
            note_sets = note_sets.update_ledger(ledger);
            released.push(withdrawal);
        }

        Ok((self, note_sets, released))
    }
}

impl ProvableOperation for ChannelWithdrawOp {
    type Proof = ZkTransferProof;
    const CODE: u8 = 0x13;
}

impl OpGasCalculator for ChannelWithdrawOp {
    // The Operation, the update of its channel note set when the withdrawal is
    // released, and the spend of its notes.
    fn execution_gas(
        &self,
        profile: &GasProfile,
        _context: &impl OpGasContext,
    ) -> Result<Gas, GasOverflow> {
        profile
            .channel_withdraw
            .checked_add(profile.note_set)?
            .checked_add(profile.spend_gas(self.inputs.len(), self.outputs.len())?)
    }
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ChannelWithdrawOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = Error;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();
        preverify_zk_transfer(&operation.inputs, operation.outputs.len())
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
        context.channels.note_set_index(operation.channel_id)?;

        // Validate the inputs against the channel note set
        let tx_commitment_root = context.tx_notes.spendable_root(
            context
                .note_sets
                .channel(&operation.channel_id)
                .expect("a channel's note set exists"),
            operation.inputs.as_slice(),
            &operation.commitment_merkle_root,
            context.block_number,
        )?;

        // Defer the proof verification so that the caller can batch it. A
        // withdrawal balances exactly.
        Ok(defer_zk_transfer(
            self.proof(),
            &operation.inputs,
            &operation.outputs,
            0,
            tx_commitment_root,
            *context.tx_hash_view.as_fr(),
        )?)
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
        let note_set_index = context.channels.note_set_index(operation.channel_id)?;

        // The inputs are spent and the outputs created when the withdrawal is
        // released.
        let due = context.block_slot.strict_add(context.withdraw_delay);
        context.pending_withdrawals = context.pending_withdrawals.push(PendingWithdrawal {
            due,
            note_set_index,
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
            outputs: [NoteCommitment(Fr::from(0x2au64))].into(),
            commitment_merkle_root: Fr::from(0x29u64),
        }
    }
}

#[cfg(test)]
mod test {
    use lb_groth16::AdditiveGroup as _;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;
    use crate::mantle::{
        gas::test_utils::{FixedThresholds, GAS_PROFILE},
        note_set::CommitmentMmr,
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

    // CHANNEL owns note set 2, which holds commitment(5), recorded at the end
    // of block 0; the ledger set holds commitment(1).
    fn state() -> (Channels, NoteSets) {
        let (channels, note_sets) =
            Channels::new().create_channel(CHANNEL, Slot::from(0u64), keys(), NoteSets::new());
        let ledger = note_sets
            .ledger()
            .clone()
            .execute_adding(&[commitment(1)])
            .unwrap();
        let channel = note_sets
            .channel(&CHANNEL)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(5)])
            .unwrap();
        (
            channels,
            note_sets
                .update_ledger(ledger)
                .update_channel(channel)
                .end_block(0, 1024)
                .unwrap(),
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
                tx_notes: &TxNoteState::new(),
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
        let ledger_root = note_sets.ledger().commitment_root();
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
                withdraw_delay: Slot::from(86_400u64),
            })
            .expect("the channel has a note set");

        assert_eq!(
            context.pending_withdrawals.iter().collect::<Vec<_>>(),
            vec![&PendingWithdrawal {
                due: Slot::from(86_410u64),
                note_set_index: NoteSetIndex::Channel(CHANNEL),
                inputs: [nullifier(5)].into(),
                outputs: [commitment(50)].into(),
            }]
        );
        assert_eq!(events, []);
    }

    #[test]
    fn channel_withdraw_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            // 1 input, 1 output, nothing buffered: 33 + 8961 + 4177 + 1758 + 58
            assert_eq!(
                ChannelWithdrawOp::sample()
                    .execution_gas(&GAS_PROFILE, &FixedThresholds(threshold)),
                Ok(Gas::new(14_987))
            );
        }
    }

    fn pending(due: u64, input: u64, output: u64) -> PendingWithdrawal {
        PendingWithdrawal {
            due: Slot::from(due),
            note_set_index: NoteSetIndex::Channel(CHANNEL),
            inputs: [nullifier(input)].into(),
            outputs: [commitment(output)].into(),
        }
    }

    #[test]
    fn release_applies_due_withdrawals_with_unspent_inputs_only() {
        let (_, note_sets) = state();
        // A step consumed nullifier(6) before its withdrawal was due.
        let channel = note_sets
            .channel(&CHANNEL)
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(6)])
            .unwrap();
        let note_sets = note_sets.update_channel(channel);
        let ledger_before = note_sets.ledger().clone();
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
        let channel = note_sets.channel(&CHANNEL).unwrap();
        assert!(channel.contains_nullifier(&nullifier(5)).unwrap());
        assert!(!channel.contains_nullifier(&nullifier(7)).unwrap());
        let expected = ledger_before.execute_adding(&[commitment(50)]).unwrap();
        assert_eq!(
            note_sets.ledger().commitment_root(),
            expected.commitment_root()
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
        assert_eq!(note_sets.ledger().commitment_root(), expected);
    }

    #[test]
    fn release_reads_only_due_withdrawals_from_a_duplicate_backlog() {
        let (_, note_sets) = state();
        let withdrawals = (0..1_024u64)
            .fold(PendingWithdrawals::new(), |withdrawals, index| {
                withdrawals.push(pending(200, 5, 1_000 + index))
            })
            .push(pending(100, 5, 200));

        assert_eq!(withdrawals.due(Slot::from(100u64)).count(), 1);

        let (withdrawals, note_sets, released) =
            withdrawals.release(note_sets, Slot::from(100u64)).unwrap();
        assert_eq!(released, vec![pending(100, 5, 200)]);
        assert_eq!(withdrawals.iter().count(), 1_024);

        // The first withdrawal spent the note; the later duplicate backlog is
        // discarded when it becomes due without creating additional outputs.
        let (withdrawals, note_sets, released) =
            withdrawals.release(note_sets, Slot::from(200u64)).unwrap();
        assert_eq!(released, []);
        assert_eq!(withdrawals.iter().count(), 0);
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(200))
            .unwrap()
            .frontier_root();
        assert_eq!(note_sets.ledger().commitment_root(), expected);
    }

    #[test]
    fn release_applies_an_sdp_withdrawal_with_no_input_behind_a_later_due_one() {
        let (_, note_sets) = state();
        // An SDP withdrawal due at the start of a later epoch, posted before a
        // channel withdrawal due earlier, and one with no input due now.
        let sdp = |due: u64, output: u64| PendingWithdrawal {
            due: Slot::from(due),
            note_set_index: NoteSetIndex::Sdp,
            inputs: InputNullifiers::default(),
            outputs: [commitment(output)].into(),
        };
        let withdrawals = PendingWithdrawals::new()
            .push(sdp(200, 40))
            .push(pending(100, 5, 50))
            .push(sdp(100, 60));
        let (withdrawals, _, released) =
            withdrawals.release(note_sets, Slot::from(100u64)).unwrap();
        assert_eq!(released, vec![pending(100, 5, 50), sdp(100, 60)]);
        assert_eq!(withdrawals.iter().collect::<Vec<_>>(), vec![&sdp(200, 40)]);
    }

    /// After skipped slots, a block releases every withdrawal that fell due
    /// since the last one, in posting order rather than due order, and keeps
    /// the posting order of the ones still pending with those posted later.
    #[test]
    fn release_after_skipped_slots_applies_every_due_withdrawal_in_posting_order() {
        let (_, note_sets) = state();
        let withdrawals = PendingWithdrawals::new()
            .push(pending(105, 5, 50))
            .push(pending(300, 6, 60))
            .push(pending(100, 7, 70))
            .push(pending(110, 8, 80))
            .push(pending(103, 9, 90));

        let (withdrawals, _, released) =
            withdrawals.release(note_sets, Slot::from(110u64)).unwrap();

        assert_eq!(
            released,
            vec![
                pending(105, 5, 50),
                pending(100, 7, 70),
                pending(110, 8, 80),
                pending(103, 9, 90),
            ]
        );
        let withdrawals = withdrawals.push(pending(200, 10, 100));
        assert_eq!(
            withdrawals.iter().collect::<Vec<_>>(),
            vec![&pending(300, 6, 60), &pending(200, 10, 100)]
        );
    }

    /// The withdrawals are encoded in posting order, and decode to the same.
    #[test]
    fn pending_withdrawals_round_trip_in_posting_order() {
        let (remaining, _, _) = PendingWithdrawals::new()
            .push(pending(100, 5, 50))
            .push(pending(300, 6, 60))
            .push(pending(200, 7, 70))
            .release(state().1, Slot::from(100u64))
            .unwrap();
        let withdrawals = remaining.push(pending(150, 8, 80));

        let decoded: PendingWithdrawals =
            serde_json::from_str(&serde_json::to_string(&withdrawals).unwrap()).unwrap();

        assert_eq!(decoded, withdrawals);
        assert_eq!(
            decoded.iter().collect::<Vec<_>>(),
            vec![
                &pending(300, 6, 60),
                &pending(200, 7, 70),
                &pending(150, 8, 80)
            ]
        );
    }
}
