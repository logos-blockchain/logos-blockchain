use std::fmt::{Debug, Formatter};

use lb_core::mantle::notes::derive_reward_nonce;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{
    ZkKey, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_log_targets::kms;
use tokio::sync::oneshot;
use tracing::debug;

const LOG_TARGET: &str = kms::operators::ZK_REWARD_NONCE;

/// The nonce of the leader reward note of the block at `slot`, under the secret
/// key of its reward key.
pub struct DeriveRewardNonce {
    slot: u64,
    result_channel: oneshot::Sender<Fr>,
}

impl Debug for DeriveRewardNonce {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeriveRewardNonce")
    }
}

impl DeriveRewardNonce {
    #[must_use]
    pub const fn new(slot: u64, result_channel: oneshot::Sender<Fr>) -> Self {
        Self {
            slot,
            result_channel,
        }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for DeriveRewardNonce {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let Self {
            slot,
            result_channel,
        } = *self;
        if result_channel
            .send(derive_reward_nonce(key.as_fr(), slot))
            .is_err()
        {
            debug!(target: LOG_TARGET, "Failed to send result via channel");
        }
        Ok(())
    }
}
