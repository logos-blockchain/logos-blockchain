use std::collections::{HashMap, HashSet};

use lb_groth16::Fr;

use crate::mantle::{
    OpRef,
    channel::Channels,
    gas::{Gas, GasOverflow, NoteSetKey, OpGasContext, ThresholdSource, root_gas},
    ops::channel::{ChannelId, ChannelKeyIndex, NULL_CHANNEL_ID, inscribe::Step},
    transactions::tx_list::ops::OpsGasContext,
};

// Where the channels an Operation executes against are read from before the
// transaction has moved them: the live channels in the ledger, the ones the
// wallet knows in the wallet estimate.
pub trait ChannelSource {
    fn configuration_threshold(&self, channel: &ChannelId) -> Option<ChannelKeyIndex>;
}

impl ChannelSource for OpsGasContext {
    fn configuration_threshold(&self, channel: &ChannelId) -> Option<ChannelKeyIndex> {
        Self::configuration_threshold(self, channel)
    }
}

impl ChannelSource for Channels {
    fn configuration_threshold(&self, channel: &ChannelId) -> Option<ChannelKeyIndex> {
        self.channels
            .get(channel)
            .map(|channel| channel.configuration_threshold)
    }
}

impl<C: ChannelSource> ChannelSource for &C {
    fn configuration_threshold(&self, channel: &ChannelId) -> Option<ChannelKeyIndex> {
        (**self).configuration_threshold(channel)
    }
}

// The state an Operation is priced against, as the transaction moves it: the
// thresholds it is verified against, the commitments buffered by the
// Operations before it, the note sets they changed and the roots their proofs
// extend. The wallet cannot observe the state its Operations will execute
// against, so it predicts it from the Operations before.
pub struct TxGasState<C: ChannelSource> {
    channels: C,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    buffered: HashMap<NoteSetKey, usize>,
    changed_note_sets: HashSet<NoteSetKey>,
    extended_roots: HashSet<(NoteSetKey, Fr, usize)>,
}

impl<C: ChannelSource> TxGasState<C> {
    #[must_use]
    pub fn new(channels: C) -> Self {
        Self {
            channels,
            configuration_thresholds: HashMap::new(),
            buffered: HashMap::new(),
            changed_note_sets: HashSet::new(),
            extended_roots: HashSet::new(),
        }
    }

    // The commitments buffered in the set by the Operations priced so far.
    #[must_use]
    pub fn buffered(&self, set: &NoteSetKey) -> usize {
        self.buffered.get(set).copied().unwrap_or(0)
    }

    // A proof extending the root over at least one buffered commitment. Proofs
    // sharing the set, the root and the count share one extended root.
    fn extend_root(&mut self, set: NoteSetKey, root: Fr, buffered: usize) {
        if buffered > 0 {
            self.extended_roots.insert((set, root, buffered));
        }
    }

    // A proof over the whole buffer of the set.
    fn extend_root_over_buffer(&mut self, set: NoteSetKey, root: Fr) {
        self.extend_root(set, root, self.buffered(&set));
    }

    // The Execution Gas of the root extensions of the proofs of the transaction,
    // each paid once.
    pub fn root_extensions_gas(&self) -> Result<Gas, GasOverflow> {
        self.extended_roots
            .iter()
            .try_fold(Gas::new(0), |total, (_, _, buffered)| {
                total.checked_add(root_gas(*buffered)?)
            })
    }

    fn channel_exists(&self, channel: &ChannelId) -> bool {
        self.configuration_thresholds.contains_key(channel)
            || self.channels.configuration_threshold(channel).is_some()
    }

    fn buffer(&mut self, set: NoteSetKey, commitments: usize) {
        let buffered = self.buffered.entry(set).or_insert(0);
        *buffered = buffered.saturating_add(commitments);
    }

    fn change(&mut self, set: NoteSetKey) {
        self.changed_note_sets.insert(set);
    }

    // A channel created by the transaction: its new note set is changed, and
    // the null channel has none.
    fn create_channel(&mut self, channel: ChannelId) {
        if channel != NULL_CHANNEL_ID {
            self.change(NoteSetKey::Channel(channel));
        }
    }

    // The number of channel and SDP note sets the transaction changed so far.
    #[must_use]
    pub fn changed_note_sets(&self) -> usize {
        self.changed_note_sets.len()
    }

    // Call once the Operation has been priced: it is itself verified against
    // the state in force before it.
    pub fn apply(&mut self, op: OpRef<'_>) {
        match op {
            OpRef::Transfer(operation) => {
                self.extend_root_over_buffer(NoteSetKey::Ledger, operation.commitment_merkle_root);
                self.buffer(NoteSetKey::Ledger, operation.outputs.as_slice().len());
            }
            OpRef::ClaimPowReward(_) => self.buffer(NoteSetKey::Ledger, 1),
            OpRef::ChannelConfig(operation) => {
                if !self.channel_exists(&operation.channel()) {
                    self.create_channel(operation.channel());
                }
                self.configuration_thresholds.insert(
                    operation.channel(),
                    operation.configuration_threshold().get(),
                );
            }
            // A deposit spends from the ledger set and adds its outputs to its
            // channel set, which it leaves unchanged when it has none.
            OpRef::ChannelDeposit(operation) => {
                let set = NoteSetKey::Channel(operation.channel_id);
                self.extend_root_over_buffer(NoteSetKey::Ledger, operation.commitment_merkle_root);
                self.buffer(set, operation.outputs.as_slice().len());
                if !operation.outputs.is_empty() {
                    self.change(set);
                }
            }
            // A withdrawal changes its channel set only when it is released, and
            // pays that update in its own gas.
            OpRef::ChannelWithdraw(operation) => {
                let set = NoteSetKey::Channel(operation.channel_id);
                self.extend_root_over_buffer(set, operation.commitment_merkle_root);
            }
            // An inscription creates the channel when it does not exist yet, and
            // changes its set when a step spends from it or adds to it. The user
            // steps and program deposits of a mini-block cover the buffer as it
            // stands when the mini-block starts, a program step the whole buffer at
            // its position. Each step then buffers the notes it creates, the
            // program outputs of a program step before its outputs.
            OpRef::ChannelInscribe(operation) => {
                let set = NoteSetKey::Channel(operation.channel_id);
                if !self.channel_exists(&operation.channel_id) {
                    self.configuration_thresholds
                        .insert(operation.channel_id, 1);
                    self.create_channel(operation.channel_id);
                }
                if operation.steps().any(moves_notes) {
                    self.change(set);
                }
                for mini_block in operation.mini_blocks.iter() {
                    let covered = self.buffered(&set);
                    for step in mini_block.iter() {
                        match step {
                            Step::User(_) | Step::ProgramDeposit(_) => {
                                self.extend_root(set, *step.commitment_merkle_root(), covered);
                            }
                            Step::Program(step) => {
                                self.extend_root_over_buffer(set, step.commitment_merkle_root);
                            }
                        }
                        self.buffer(set, step.outputs().len());
                    }
                }
            }
            OpRef::SDPDeclare(operation) => {
                self.extend_root_over_buffer(NoteSetKey::Ledger, operation.commitment_merkle_root);
                self.change(NoteSetKey::Sdp);
            }
            OpRef::SDPWithdraw(_) => self.change(NoteSetKey::Sdp),
            OpRef::SDPActive(_) | OpRef::ProgramRegister(_) => {}
        }
    }
}

// Whether the step spends from or adds to the channel set.
fn moves_notes(step: &Step) -> bool {
    !step.inputs().is_empty() || !step.outputs().is_empty()
}

impl<C: ChannelSource> ThresholdSource for TxGasState<C> {
    fn configuration_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex {
        self.configuration_thresholds
            .get(channel)
            .copied()
            .or_else(|| self.channels.configuration_threshold(channel))
            .unwrap_or(0)
    }
}

impl<C: ChannelSource> OpGasContext for TxGasState<C> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mantle::{
        Op,
        notes::{InputNullifiers, NoteNullifier, OutputCommitments},
        ops::{
            channel::{
                config::ChannelConfigOp,
                deposit::DepositOp,
                inscribe::{Inscription, InscriptionOp, MiniBlock, MiniBlocks, UserStep},
                program_step::{ProgramDeposit, ProgramStep},
                withdraw::ChannelWithdrawOp,
            },
            pow::ClaimPowRewardOp,
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
        transactions::GasPrices,
    };

    fn empty() -> OpsGasContext {
        OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0))
    }

    fn existing(channel: ChannelId, threshold: ChannelKeyIndex) -> OpsGasContext {
        OpsGasContext::new([(channel, threshold)].into(), GasPrices::new(0, 0))
    }

    fn user_step(root: u64, nullifier: u64, outputs: u64) -> Step {
        Step::User(UserStep {
            inputs: [NoteNullifier(Fr::from(nullifier))].into(),
            outputs: OutputCommitments::try_from_iter((0..outputs).map(|output| {
                crate::mantle::notes::NoteCommitment(Fr::from(100 + nullifier + output))
            }))
            .unwrap(),
            commitment_merkle_root: Fr::from(root),
            inscription: Inscription::default(),
        })
    }

    fn inscription_of(mini_blocks: Vec<Vec<Step>>) -> Op {
        Op::ChannelInscribe(InscriptionOp {
            mini_blocks: MiniBlocks::try_from(
                mini_blocks
                    .into_iter()
                    .map(|steps| MiniBlock::try_from(steps).unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            ..InscriptionOp::sample()
        })
    }

    #[test]
    fn steps_of_a_mini_block_sharing_their_root_pay_one_extension() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(
            inscription_of(vec![
                vec![user_step(1, 1, 4)],
                vec![user_step(7, 2, 0), user_step(7, 3, 0), user_step(7, 4, 0)],
            ])
            .by_ref(),
        );
        assert_eq!(state.root_extensions_gas(), root_gas(4));
    }

    #[test]
    fn steps_differing_by_root_or_mini_block_pay_one_extension_each() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(
            inscription_of(vec![
                vec![user_step(1, 1, 4)],
                vec![user_step(7, 2, 1), user_step(8, 3, 0)],
                vec![user_step(7, 4, 0)],
            ])
            .by_ref(),
        );
        let expected = root_gas(4)
            .unwrap()
            .checked_add(root_gas(4).unwrap())
            .unwrap()
            .checked_add(root_gas(5).unwrap())
            .unwrap();
        assert_eq!(state.root_extensions_gas(), Ok(expected));
    }

    #[test]
    fn steps_of_a_first_mini_block_over_nothing_buffered_extend_no_root() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(inscription_of(vec![vec![user_step(7, 1, 1), user_step(8, 2, 1)]]).by_ref());
        assert_eq!(state.root_extensions_gas(), Ok(Gas::new(0)));
    }

    #[test]
    fn a_first_mini_block_covers_the_buffer_of_the_earlier_operations() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let inscription = InscriptionOp::sample();
        let deposit = Op::ChannelDeposit(DepositOp {
            channel_id: inscription.channel_id,
            ..DepositOp::sample()
        });
        state.apply(deposit.by_ref());
        let deposited = state.buffered(&NoteSetKey::Channel(inscription.channel_id));
        assert!(deposited > 0);
        let before = state.root_extensions_gas().unwrap();
        state.apply(inscription_of(vec![vec![user_step(7, 1, 0)]]).by_ref());
        assert_eq!(
            state.root_extensions_gas(),
            Ok(before.checked_add(root_gas(deposited).unwrap()).unwrap())
        );
    }

    #[test]
    fn transfers_extend_their_root_over_the_whole_buffer_of_the_ledger() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let transfer = Op::Transfer(TransferOp::sample()); // two outputs
        state.apply(transfer.by_ref());
        assert_eq!(state.root_extensions_gas(), Ok(Gas::new(0)));
        state.apply(transfer.by_ref());
        assert_eq!(state.root_extensions_gas(), root_gas(2));
        // the buffer has grown, so the root is extended over more commitments
        state.apply(transfer.by_ref());
        let expected = root_gas(2)
            .unwrap()
            .checked_add(root_gas(4).unwrap())
            .unwrap();
        assert_eq!(state.root_extensions_gas(), Ok(expected));
    }

    #[test]
    fn transfer_outputs_are_buffered_after_it() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let op = Op::Transfer(TransferOp::sample());
        assert_eq!(state.buffered(&NoteSetKey::Ledger), 0);
        state.apply(op.by_ref());
        assert_eq!(state.buffered(&NoteSetKey::Ledger), 2);
        assert_eq!(state.changed_note_sets(), 0);
    }

    #[test]
    fn claim_pow_reward_buffers_one_note_in_the_ledger() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(Op::ClaimPowReward(ClaimPowRewardOp::sample()).by_ref());
        assert_eq!(state.buffered(&NoteSetKey::Ledger), 1);
        assert_eq!(state.changed_note_sets(), 0);
    }

    #[test]
    fn deposit_buffers_its_outputs_in_its_channel() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let deposit = DepositOp::sample();
        let set = NoteSetKey::Channel(deposit.channel_id);
        state.apply(Op::ChannelDeposit(deposit).by_ref());
        assert_eq!(state.buffered(&set), 1);
        assert_eq!(state.buffered(&NoteSetKey::Ledger), 0);
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn inscription_buffers_the_notes_its_steps_create() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let inscription = InscriptionOp::sample_with_step();
        let set = NoteSetKey::Channel(inscription.channel_id);
        let expected: usize = inscription.steps().map(|step| step.outputs().len()).sum();
        assert!(expected > 0);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.buffered(&set), expected);
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn a_program_step_covers_the_buffer_at_its_position() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        // The deposit of the mini-block covers nothing buffered, the program step
        // the change of the deposit, then the next deposit and program step the
        // change and the program note.
        let deposit = Step::ProgramDeposit(ProgramDeposit::sample());
        let program = Step::Program(ProgramStep::sample());
        state.apply(
            inscription_of(vec![
                vec![deposit.clone(), program.clone()],
                vec![deposit, program],
            ])
            .by_ref(),
        );
        let expected = root_gas(1)
            .unwrap()
            .checked_add(root_gas(2).unwrap())
            .unwrap()
            .checked_add(root_gas(3).unwrap())
            .unwrap();
        assert_eq!(state.root_extensions_gas(), Ok(expected));
    }

    #[test]
    fn config_on_an_existing_channel_changes_no_set() {
        let config = ChannelConfigOp::sample();
        let context = existing(config.channel(), 2);
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelConfig(config).by_ref());
        assert_eq!(state.changed_note_sets(), 0);
    }

    #[test]
    fn config_creating_a_channel_changes_its_set() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelConfig(ChannelConfigOp::sample()).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn inscription_without_steps_on_an_existing_channel_changes_no_set() {
        let inscription = InscriptionOp::sample();
        let context = existing(inscription.channel_id, 1);
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.changed_note_sets(), 0);
    }

    #[test]
    fn inscription_without_steps_creating_a_channel_changes_its_set() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelInscribe(InscriptionOp::sample()).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn deposit_without_outputs_changes_no_set() {
        let deposit = DepositOp {
            outputs: OutputCommitments::default(),
            ..DepositOp::sample()
        };
        let set = NoteSetKey::Channel(deposit.channel_id);
        let context = existing(deposit.channel_id, 1);
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelDeposit(deposit).by_ref());
        assert_eq!(state.buffered(&set), 0);
        assert_eq!(state.changed_note_sets(), 0);
    }

    // A program step that spends and creates nothing, proven against a recent
    // root.
    fn empty_program_step() -> ProgramStep {
        ProgramStep {
            inputs: InputNullifiers::default(),
            program_outputs: OutputCommitments::default(),
            outputs: OutputCommitments::default(),
            ..ProgramStep::sample()
        }
    }

    fn inscription_with(steps: Vec<Step>) -> InscriptionOp {
        InscriptionOp {
            mini_blocks: [MiniBlock::try_from(steps).expect("a mini-block has a step")].into(),
            ..InscriptionOp::sample()
        }
    }

    #[test]
    fn inscription_with_an_empty_step_on_an_existing_channel_changes_no_set() {
        let inscription = inscription_with(vec![Step::Program(empty_program_step())]);
        let context = existing(inscription.channel_id, 1);
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.changed_note_sets(), 0);
    }

    #[test]
    fn creating_the_null_channel_changes_no_set() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let inscription = InscriptionOp {
            channel_id: NULL_CHANNEL_ID,
            ..InscriptionOp::sample()
        };
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.changed_note_sets(), 0);
    }

    #[test]
    fn inscription_with_an_empty_step_creating_a_channel_changes_its_set() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let inscription = inscription_with(vec![Step::Program(empty_program_step())]);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn inscription_with_a_step_spending_only_changes_its_set() {
        let step = UserStep {
            inputs: [NoteNullifier(Fr::from(0x23u64))].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: Fr::from(0x25u64),
            inscription: Inscription::default(),
        };
        let inscription = inscription_with(vec![Step::User(step)]);
        let context = existing(inscription.channel_id, 1);
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn inscription_with_a_program_deposit_spending_only_changes_its_set() {
        let deposit = ProgramDeposit {
            outputs: OutputCommitments::default(),
            ..ProgramDeposit::sample()
        };
        let inscription = inscription_with(vec![
            Step::ProgramDeposit(deposit),
            Step::Program(empty_program_step()),
        ]);
        let context = existing(inscription.channel_id, 1);
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn withdraw_changes_no_set() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelWithdraw(ChannelWithdrawOp::sample()).by_ref());
        assert_eq!(state.changed_note_sets(), 0);
    }

    #[test]
    fn two_operations_on_one_channel_count_once() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let inscription = InscriptionOp {
            channel_id: DepositOp::sample().channel_id,
            ..InscriptionOp::sample_with_step()
        };
        state.apply(Op::ChannelDeposit(DepositOp::sample()).by_ref());
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn sdp_operations_change_the_sdp_set() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(Op::SDPActive(SDPActiveOp::sample()).by_ref());
        assert_eq!(state.changed_note_sets(), 0);
        state.apply(Op::SDPDeclare(SDPDeclareOp::sample()).by_ref());
        state.apply(Op::SDPWithdraw(SDPWithdrawOp::sample()).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
        assert_eq!(state.buffered(&NoteSetKey::Sdp), 0);
    }

    #[test]
    fn channel_config_moves_the_threshold_for_the_operations_after_it() {
        let config = ChannelConfigOp::sample();
        let channel = config.channel();
        let threshold = config.configuration_threshold().get();
        let context = empty();
        let mut state = TxGasState::new(&context);
        assert_eq!(state.configuration_threshold(&channel), 0);
        state.apply(Op::ChannelConfig(config).by_ref());
        assert_eq!(state.configuration_threshold(&channel), threshold);
    }

    #[test]
    fn inscription_creating_a_channel_sets_its_threshold_to_one() {
        let inscription = InscriptionOp::sample();
        let channel = inscription.channel_id;
        let context = empty();
        let mut state = TxGasState::new(&context);
        assert_eq!(state.configuration_threshold(&channel), 0);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.configuration_threshold(&channel), 1);
    }

    #[test]
    fn inscription_does_not_move_the_threshold_of_an_existing_channel() {
        let inscription = InscriptionOp::sample();
        let channel = inscription.channel_id;
        let context = existing(channel, 3);
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.configuration_threshold(&channel), 3);
    }
}
