use core::fmt::{self, Debug, Formatter};

use tokio::sync::oneshot;
use tracing::error;

use crate::keys::{
    Ed25519Key, UnsecuredEd25519Key, errors::KeyError, secured_key::SecureKeyOperator,
};

pub struct ExfiltrateSecretKeyOperator {
    response_channel: oneshot::Sender<UnsecuredEd25519Key>,
}

impl Debug for ExfiltrateSecretKeyOperator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExfiltrateSecretKeyOperator").finish()
    }
}

impl ExfiltrateSecretKeyOperator {
    #[must_use]
    pub const fn new(response_channel: oneshot::Sender<UnsecuredEd25519Key>) -> Self {
        Self { response_channel }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for ExfiltrateSecretKeyOperator {
    type Key = Ed25519Key;
    type Error = KeyError;

    async fn execute(mut self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let _ = self
            .response_channel
            .send(key.clone().into_unsecured())
            .map_err(|_| error!("Error sending Ed25519 key to requester."));
        Ok(())
    }
}
