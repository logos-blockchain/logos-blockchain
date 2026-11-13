use std::fmt::Debug;

use lb_blend_proofs::{
    CorePathAndSelectors,
    quota::{
        self, KeyIndex, VerifiedProofOfQuota,
        inputs::prove::{PrivateInputs, PublicInputs, private::ProofOfCoreQuotaInputs},
    },
};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{
    ZkKey,
    secured_key::{SecureKeyOperator, SecuredKey},
};
use lb_log_targets::kms;
use lb_utils::tokio::task::spawn_blocking;
use tokio::sync::oneshot;
use tracing::trace;

const LOG_TARGET: &str = kms::operators::BLEND_POQ;

pub struct PoQOperator {
    core_path_and_selectors: CorePathAndSelectors,
    public_inputs: PublicInputs,
    key_index: KeyIndex,
    response_channel: oneshot::Sender<Result<(VerifiedProofOfQuota, Fr), quota::Error>>,
}

impl Debug for PoQOperator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PoQOperator")
    }
}

impl PoQOperator {
    #[must_use]
    pub const fn new(
        core_path_and_selectors: CorePathAndSelectors,
        public_inputs: PublicInputs,
        key_index: KeyIndex,
        response_channel: oneshot::Sender<Result<(VerifiedProofOfQuota, Fr), quota::Error>>,
    ) -> Self {
        Self {
            core_path_and_selectors,
            public_inputs,
            key_index,
            response_channel,
        }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for PoQOperator {
    type Key = ZkKey;
    type Error = <ZkKey as SecuredKey>::Error;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let Self {
            core_path_and_selectors,
            public_inputs,
            key_index,
            response_channel,
        } = *self;
        let private_inputs = PrivateInputs::new_proof_of_core_quota_inputs(
            key_index,
            ProofOfCoreQuotaInputs {
                core_path_and_selectors,
                core_sk: *key.as_fr(),
            },
        );
        // spawn a blocking task as this computation is heavy atm because it
        // needs of an external binary. Not awaited, so that the KMS can
        // serve others meanwhile.
        spawn_blocking("logos/blend/core-poq-blocking", move || {
            let poq_result = VerifiedProofOfQuota::new(&public_inputs, private_inputs);
            drop(response_channel.send(poq_result).inspect_err(|_| {
                trace!(target: LOG_TARGET, "Error sending generated proof of quota, most likely due to an epoch rotation that discarded the receiver side of the channel.");
            }));
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use lb_blend_proofs::quota::{Quota, fixtures::valid_proof_of_core_quota_inputs};
    use lb_key_management_system_keys::keys::Ed25519Key;
    use tokio::{runtime::Builder, time::timeout};

    use super::*;

    #[test]
    fn execute_returns_before_the_proof_starts() {
        let (public_inputs, private_inputs) = valid_proof_of_core_quota_inputs(
            Ed25519Key::from_bytes(&[1; 32]).public_key().into_inner(),
            Quota::ONE,
        );
        let (response_channel, response) = oneshot::channel();
        let operator = Box::new(PoQOperator::new(
            private_inputs.core_path_and_selectors,
            public_inputs,
            KeyIndex::new::<0>(),
            response_channel,
        ));
        let key = ZkKey::new(private_inputs.core_sk);
        let runtime = Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap();

        runtime.block_on(async {
            // Takes the only blocking thread, so the proof can't start.
            let (release, stalled) = mpsc::channel::<()>();
            spawn_blocking("test/stall", move || stalled.recv());

            timeout(Duration::from_secs(5), operator.execute(&key))
                .await
                .expect("`execute` should not wait for the proof")
                .unwrap();

            drop(release);
            assert!(response.await.unwrap().is_ok());
        });
    }
}
