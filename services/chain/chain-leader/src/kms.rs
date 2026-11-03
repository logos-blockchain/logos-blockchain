use std::fmt::{Debug, Display};

use lb_core::{
    mantle::Note,
    proofs::leader_proof::{LeaderPrivate, LeaderPublic},
};
use lb_groth16::Fr;
use lb_key_management_system_service::{
    KMSService,
    api::KmsServiceApi,
    backend::preload::{KeyId, PreloadKMSBackend},
    keys::KeyOperators,
    operators::zk::leader::{BuildPrivateInputsWithLeaderKey, CheckLotteryWinning},
};
use lb_pol::PolWalletInputsData;
use overwatch::{DynError, services::AsServiceId};
use tokio::sync::oneshot;

use crate::leadership::PrivateInputsError;

pub type PreloadKmsService<RuntimeServiceId> = KMSService<PreloadKMSBackend, RuntimeServiceId>;

#[async_trait::async_trait]
pub trait KmsAdapter<RuntimeServiceId> {
    type KeyId;

    async fn check_winning_with_key(
        &self,
        key_id: Self::KeyId,
        note: &Note,
        public_inputs: &LeaderPublic,
    ) -> Result<bool, DynError>;

    async fn build_private_inputs(
        &self,
        key_id: Self::KeyId,
        public_inputs: LeaderPublic,
        wallet: PolWalletInputsData,
        msg: Fr,
    ) -> Result<LeaderPrivate, PrivateInputsError>;
}

#[async_trait::async_trait]
impl<RuntimeServiceId> KmsAdapter<RuntimeServiceId>
    for KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>
where
    RuntimeServiceId:
        AsServiceId<PreloadKmsService<RuntimeServiceId>> + Debug + Display + Send + Sync + 'static,
{
    type KeyId = KeyId;

    async fn check_winning_with_key(
        &self,
        key_id: Self::KeyId,
        note: &Note,
        public_inputs: &LeaderPublic,
    ) -> Result<bool, DynError> {
        let (output_tx, output_rx) = oneshot::channel();
        // clone to send
        let note = *note;
        let public_inputs = *public_inputs;
        Box::pin(self.execute(
            key_id,
            KeyOperators::Zk(Box::new(CheckLotteryWinning::new(
                output_tx,
                note,
                public_inputs,
            ))),
        ))
        .await?;

        Ok(output_rx
            .await
            .map_err(|_| "KMS API did not respond to the lottery winning check")?)
    }

    async fn build_private_inputs(
        &self,
        key_id: Self::KeyId,
        public_inputs: LeaderPublic,
        wallet: PolWalletInputsData,
        msg: Fr,
    ) -> Result<LeaderPrivate, PrivateInputsError> {
        let (output_tx, output_rx) = oneshot::channel();
        let operator = BuildPrivateInputsWithLeaderKey::new(output_tx, public_inputs, wallet, msg);
        Box::pin(self.execute(key_id, KeyOperators::Zk(Box::new(operator))))
            .await
            .map_err(|error| PrivateInputsError::KmsApi(Box::new(error)))?;

        output_rx.await.map_err(|_| PrivateInputsError::KmsResponse)
    }
}
