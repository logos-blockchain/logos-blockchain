use lb_core::mantle::{
    ops::channel::{ChannelId, ChannelKeyIndex},
    tx::{OperationVerificationHelper, VerificationError},
};
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::mantle::LedgerState;

pub struct MantleOperationVerificationHelper<'a> {
    ledger_state: &'a LedgerState,
}

impl<'a> MantleOperationVerificationHelper<'a> {
    #[must_use]
    pub const fn new(ledger_state: &'a LedgerState) -> Self {
        Self { ledger_state }
    }
}

impl OperationVerificationHelper for MantleOperationVerificationHelper<'_> {
    fn get_channel_withdraw_threshold(
        &self,
        channel_id: &ChannelId,
    ) -> Result<ChannelKeyIndex, VerificationError> {
        self.ledger_state
            .channels()
            .channel_state(channel_id)
            .ok_or(VerificationError::ChannelNotFound {
                channel_id: *channel_id,
            })
            .map(|channel_state| channel_state.withdraw_threshold)
    }

    fn get_key_from_channel_at_index(
        &self,
        channel_id: &ChannelId,
        key_index: &ChannelKeyIndex,
    ) -> Result<Ed25519PublicKey, VerificationError> {
        self.ledger_state
            .channels()
            .channel_state(channel_id)
            .ok_or(VerificationError::ChannelNotFound {
                channel_id: *channel_id,
            })?
            .accredited_keys
            .get(*key_index as usize)
            .ok_or(VerificationError::KeyNotFound {
                channel_id: *channel_id,
                key_index: *key_index,
            })
            .cloned()
    }
}
