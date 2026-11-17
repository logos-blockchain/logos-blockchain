use core::fmt::{self, Debug, Formatter};

use lb_key_management_system_keys::keys::{
    ZkKey, ZkPublicKey, errors::KeyError, kdf_public_key, secured_key::SecureKeyOperator,
};
use lb_log_targets::kms;
use tokio::sync::oneshot;
use tracing::debug;

const LOG_TARGET: &str = kms::operators::BLEND_POQ;

/// Derives the `zk_id` of a quota secret key, `zkhash(KDF, core_sk)`, the key
/// whose secret the Proof of Quota proves. It is not the public key of the
/// notes of the key, which derives from its nullifier key.
pub struct ZkIdOperator {
    response_channel: oneshot::Sender<ZkPublicKey>,
}

impl Debug for ZkIdOperator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZkIdOperator").finish()
    }
}

impl ZkIdOperator {
    #[must_use]
    pub const fn new(response_channel: oneshot::Sender<ZkPublicKey>) -> Self {
        Self { response_channel }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for ZkIdOperator {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        if self
            .response_channel
            .send(kdf_public_key(key.as_fr()))
            .is_err()
        {
            debug!(target: LOG_TARGET, "Failed to send the zk_id via channel");
        }
        Ok(())
    }
}
