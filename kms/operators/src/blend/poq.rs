use std::fmt::Debug;

use lb_blend_proofs::{
    CorePathAndSelectors,
    quota::{
        self, VerifiedProofOfQuota,
        inputs::prove::{PrivateInputs, PublicInputs, private::ProofOfCoreQuotaInputs},
    },
};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{
    ZkKey,
    secured_key::{SecureKeyOperator, SecuredKey},
};
use tokio::{sync::oneshot, task::spawn_blocking};
use tracing::error;

pub struct PoQOperator {
    core_path_and_selectors: CorePathAndSelectors,
    public_inputs: PublicInputs,
    key_index: u64,
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
        key_index: u64,
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

    async fn execute(mut self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let private_inputs = PrivateInputs::new_proof_of_core_quota_inputs(
            self.key_index,
            ProofOfCoreQuotaInputs {
                core_path_and_selectors: self.core_path_and_selectors,
                core_sk: *key.as_fr(),
            },
        );
        let public_inputs = self.public_inputs;
        // spawn a blocking task as this computation is heavy atm because it needs of an
        // external binary.
        let poq_result =
            spawn_blocking(move || VerifiedProofOfQuota::new(&public_inputs, private_inputs))
                .await
                .map_err(Self::Error::FailedOperatorCall)?;
        if let Err(e) = self.response_channel.send(poq_result) {
            error!("Error building proof of quota: {e:?}");
        }
        Ok(())
    }
}
