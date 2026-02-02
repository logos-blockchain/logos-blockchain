use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{
    ZkKey, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher as ZkHasher, ZkHash};
use tokio::sync::oneshot;
use tracing::error;

pub struct VoucherOperator {
    index: Fr,
    result_channel: oneshot::Sender<ZkHash>,
}

#[async_trait::async_trait]
impl SecureKeyOperator for VoucherOperator {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let voucher = ZkHasher::digest(&[*key.as_fr(), self.index]);
        if self.result_channel.send(voucher).is_err() {
            error!("Failed to send voucher via channel");
        }
        Ok(())
    }
}
