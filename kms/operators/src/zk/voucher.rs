use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{
    ZkKey, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher as ZkHasher};
use tokio::sync::oneshot;
use tracing::error;

/// Derives/returns a voucher secret from key and index.
// TODO: Make this secure by embedding actual logic
//       once resolving cyclic dep: kms-keys <> core
#[derive(Debug)]
pub struct UnsafeVoucherOperator {
    index: Fr,
    result_channel: oneshot::Sender<Fr>,
}

impl UnsafeVoucherOperator {
    #[must_use]
    pub const fn new(index: Fr, result_channel: oneshot::Sender<Fr>) -> Self {
        Self {
            index,
            result_channel,
        }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for UnsafeVoucherOperator {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let voucher_secret = ZkHasher::digest(&[*key.as_fr(), self.index]);
        if self.result_channel.send(voucher_secret).is_err() {
            error!("Failed to send voucher via channel");
        }
        Ok(())
    }
}
