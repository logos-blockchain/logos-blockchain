use core::fmt::{self, Debug, Formatter};

use lb_key_management_system_keys::keys::{
    Ed25519Key, UnsecuredEd25519Key, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_log_targets::kms;
use tokio::sync::oneshot;
use tracing::debug;

const LOG_TARGET: &str = kms::operators::ED25519;

pub struct LeakSecretKeyOperator {
    response_channel: oneshot::Sender<UnsecuredEd25519Key>,
}

impl Debug for LeakSecretKeyOperator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeakSecretKeyOperator").finish()
    }
}

impl LeakSecretKeyOperator {
    #[must_use]
    pub const fn new(response_channel: oneshot::Sender<UnsecuredEd25519Key>) -> Self {
        Self { response_channel }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for LeakSecretKeyOperator {
    type Key = Ed25519Key;
    type Error = KeyError;

    async fn execute(mut self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let _ = self
            .response_channel
            .send(key.clone().into_unsecured())
            .map_err(|_| debug!(target: LOG_TARGET, "Error sending Ed25519 key to requester."));
        Ok(())
    }
}
