use std::collections::HashMap;

use crate::mantle::{
    OpRef,
    gas::ThresholdSource,
    ops::channel::{ChannelId, ChannelKeyIndex},
    transactions::tx_list::ops::OpsGasContext,
};

// The thresholds an Operation is verified against, as the transaction moves
// them. The wallet cannot observe the state its Operations will execute
// against, so it predicts it from the ones that create or configure a channel.
pub struct RunningThresholds<'a> {
    context: &'a OpsGasContext,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
}

impl<'a> RunningThresholds<'a> {
    #[must_use]
    pub fn new(context: &'a OpsGasContext) -> Self {
        Self {
            context,
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
                self.configuration_thresholds.insert(
                    operation.channel(),
                    operation.configuration_threshold().get(),
                );
            }
            // An inscription creates the channel when it does not exist yet.
            OpRef::ChannelInscribe(operation) => {
                if !self.channel_exists(&operation.channel_id) {
                    self.configuration_thresholds
                        .insert(operation.channel_id, 1);
                }
            }
            OpRef::ChannelDeposit(_)
            | OpRef::ChannelWithdraw(_)
            | OpRef::SDPDeclare(_)
            | OpRef::SDPWithdraw(_)
            | OpRef::SDPActive(_)
            | OpRef::Transfer(_)
            | OpRef::ClaimPowReward(_)
            | OpRef::ProgramRegister(_) => {}
        }
    }
}

impl ThresholdSource for RunningThresholds<'_> {
    fn configuration_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex {
        self.configuration_thresholds
            .get(channel)
            .copied()
            .or_else(|| self.context.configuration_threshold(channel))
            .unwrap_or(0)
    }
}
