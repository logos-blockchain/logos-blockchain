use std::collections::HashMap;

use crate::mantle::{
    OpRef,
    channel::DEFAULT_TRANSFER_THRESHOLD,
    gas::ThresholdSource,
    ops::channel::{ChannelId, ChannelKeyIndex},
    transactions::tx_list::ops::OpsGasContext,
};

// The thresholds an Operation is verified against, as the transaction moves
// them. The wallet cannot observe the state its Operations will execute
// against, so it predicts it from the ones that create or configure a channel.
pub struct RunningThresholds<'a> {
    context: &'a OpsGasContext,
    transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
}

impl<'a> RunningThresholds<'a> {
    #[must_use]
    pub fn new(context: &'a OpsGasContext) -> Self {
        Self {
            context,
            transfer_thresholds: HashMap::new(),
            configuration_thresholds: HashMap::new(),
        }
    }

    fn channel_exists(&self, channel: &ChannelId) -> bool {
        self.configuration_thresholds.contains_key(channel)
            || self.context.configuration_threshold(channel).is_some()
    }

    // Call once the Operation has been priced: it is itself verified against
    // the thresholds in force before it.
    pub fn apply(&mut self, op: OpRef<'_>) {
        match op {
            OpRef::ChannelConfig(operation) => {
                self.transfer_thresholds
                    .insert(operation.channel, operation.transfer_threshold);
                self.configuration_thresholds
                    .insert(operation.channel, operation.configuration_threshold);
            }
            // An inscription creates the channel when it does not exist yet.
            OpRef::ChannelInscribe(operation) => {
                if !self.channel_exists(&operation.channel_id) {
                    self.transfer_thresholds
                        .insert(operation.channel_id, DEFAULT_TRANSFER_THRESHOLD);
                    self.configuration_thresholds
                        .insert(operation.channel_id, 1);
                }
            }
            OpRef::ChannelDeposit(_)
            | OpRef::ChannelWithdraw(_)
            | OpRef::ChannelTransfer(_)
            | OpRef::SDPDeclare(_)
            | OpRef::SDPWithdraw(_)
            | OpRef::SDPActive(_)
            | OpRef::LeaderClaim(_)
            | OpRef::Transfer(_)
            | OpRef::ClaimPowReward(_) => {}
        }
    }
}

impl ThresholdSource for RunningThresholds<'_> {
    fn transfer_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex {
        self.transfer_thresholds
            .get(channel)
            .copied()
            .or_else(|| self.context.transfer_threshold(channel))
            .unwrap_or(0)
    }

    fn configuration_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex {
        self.configuration_thresholds
            .get(channel)
            .copied()
            .or_else(|| self.context.configuration_threshold(channel))
            .unwrap_or(0)
    }
}
