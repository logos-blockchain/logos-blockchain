use std::collections::{HashMap, HashSet};

use crate::mantle::{
    OpRef,
    channel::Channels,
    gas::{NoteSetKey, OpGasContext, ThresholdSource},
    ops::channel::{ChannelId, ChannelKeyIndex, inscribe::Step},
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
// Operations before it and the note sets they changed. The wallet cannot
// observe the state its Operations will execute against, so it predicts it
// from the Operations before.
pub struct TxGasState<C: ChannelSource> {
    channels: C,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    buffered: HashMap<NoteSetKey, usize>,
    changed_note_sets: HashSet<NoteSetKey>,
}

impl<C: ChannelSource> TxGasState<C> {
    #[must_use]
    pub fn new(channels: C) -> Self {
        Self {
            channels,
            configuration_thresholds: HashMap::new(),
            buffered: HashMap::new(),
            changed_note_sets: HashSet::new(),
        }
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
                self.buffer(NoteSetKey::Ledger, operation.outputs.as_slice().len());
            }
            OpRef::ClaimPowReward(_) => self.buffer(NoteSetKey::Ledger, 1),
            OpRef::ChannelConfig(operation) => {
                if !self.channel_exists(&operation.channel) {
                    self.change(NoteSetKey::Channel(operation.channel));
                }
                self.configuration_thresholds
                    .insert(operation.channel, operation.configuration_threshold);
            }
            OpRef::ChannelDeposit(operation) => {
                let set = NoteSetKey::Channel(operation.channel_id);
                self.buffer(set, operation.outputs.as_slice().len());
                self.change(set);
            }
            OpRef::ChannelWithdraw(operation) => {
                self.change(NoteSetKey::Channel(operation.channel_id));
            }
            // An inscription creates the channel when it does not exist yet. Its
            // steps buffer the outputs of each payment of a program step, then
            // the outputs of the step, in order.
            OpRef::ChannelInscribe(operation) => {
                let set = NoteSetKey::Channel(operation.channel_id);
                if !self.channel_exists(&operation.channel_id) {
                    self.configuration_thresholds
                        .insert(operation.channel_id, 1);
                    self.change(set);
                }
                if !operation.steps.is_empty() {
                    self.change(set);
                }
                for step in operation.steps.iter() {
                    if let Step::Program(step) = step {
                        for payment in step.payments.iter() {
                            self.buffer(set, payment.outputs.as_slice().len());
                        }
                    }
                    self.buffer(set, step.outputs().as_slice().len());
                }
            }
            OpRef::SDPDeclare(_) | OpRef::SDPWithdraw(_) => self.change(NoteSetKey::Sdp),
            OpRef::SDPActive(_) | OpRef::ProgramRegister(_) => {}
        }
    }
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

impl<C: ChannelSource> OpGasContext for TxGasState<C> {
    fn buffered(&self, set: &NoteSetKey) -> usize {
        self.buffered.get(set).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mantle::{
        Op,
        ops::{
            channel::{
                config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp,
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
    fn inscription_buffers_payments_then_steps() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        let inscription = InscriptionOp::sample_with_step();
        let set = NoteSetKey::Channel(inscription.channel_id);
        let expected: usize = inscription
            .steps
            .iter()
            .map(|step| {
                let payments = match step {
                    Step::Program(step) => step
                        .payments
                        .iter()
                        .map(|payment| payment.outputs.as_slice().len())
                        .sum(),
                    Step::User(_) => 0,
                };
                payments + step.outputs().as_slice().len()
            })
            .sum();
        assert!(expected > 0);
        state.apply(Op::ChannelInscribe(inscription).by_ref());
        assert_eq!(state.buffered(&set), expected);
        assert_eq!(state.changed_note_sets(), 1);
    }

    #[test]
    fn config_on_an_existing_channel_changes_no_set() {
        let config = ChannelConfigOp::sample();
        let context = existing(config.channel, 2);
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
    fn withdraw_changes_its_channel_set() {
        let context = empty();
        let mut state = TxGasState::new(&context);
        state.apply(Op::ChannelWithdraw(ChannelWithdrawOp::sample()).by_ref());
        assert_eq!(state.changed_note_sets(), 1);
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
        let channel = config.channel;
        let context = empty();
        let mut state = TxGasState::new(&context);
        assert_eq!(state.configuration_threshold(&channel), 0);
        state.apply(Op::ChannelConfig(config).by_ref());
        assert_eq!(state.configuration_threshold(&channel), 12);
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
