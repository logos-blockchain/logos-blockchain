use core::fmt::{self, Debug, Formatter};

use lb_key_management_system_keys::keys::{
    Ed25519Key, X25519PrivateKey, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_log_targets::kms;
use tokio::sync::oneshot;
use tracing::debug;

const LOG_TARGET: &str = kms::operators::ED25519;

pub struct DeriveX25519Operator {
    response_channel: oneshot::Sender<X25519PrivateKey>,
}

impl Debug for DeriveX25519Operator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "DeriveX25519Operator")
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for DeriveX25519Operator {
    type Key = Ed25519Key;
    type Error = KeyError;

    async fn execute(mut self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let x25519_secret_key = key.derive_x25519();
        let _ = self
            .response_channel
            .send(x25519_secret_key)
            .map_err(|_| debug!(target: LOG_TARGET, "Error sending X25519 secret key."));
        Ok(())
    }
}
