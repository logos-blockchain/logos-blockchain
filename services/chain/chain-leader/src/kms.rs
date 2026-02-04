use std::fmt::{Debug, Display};

use lb_core::{
    mantle::Utxo,
    proofs::leader_proof::{LeaderPrivate, LeaderPublic},
};
use lb_key_management_system_service::{
    KMSService,
    api::KmsServiceApi,
    backend::preload::{KeyId, PreloadKMSBackend},
    keys::{Ed25519Key, KeyOperators},
    operators::zk::leader::CheckLotteryWinning,
};
use lb_ledger::{EpochState, UtxoTree};
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;

use crate::leadership::{
    PrivateInputsError, operator_for_private_inputs_arguments_for_winning_utxo_and_slot,
};

pub type PreloadKmsService<RuntimeServiceId> = KMSService<PreloadKMSBackend, RuntimeServiceId>;

#[async_trait::async_trait]
pub trait KmsAdapter<RuntimeServiceId> {
    type KeyId;

    async fn check_winning_with_key(
        &self,
        key_id: Self::KeyId,
        utxo: &Utxo,
        public_inputs: &LeaderPublic,
    ) -> bool;

    async fn build_private_inputs_for_winning_utxo_and_slot(
        &self,
        key_id: Self::KeyId,
        utxo: &Utxo,
        epoch_state: &EpochState,
        public_inputs: LeaderPublic,
        latest_tree: &UtxoTree,
    ) -> Result<(LeaderPrivate, Ed25519Key), PrivateInputsError>;
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
        utxo: &Utxo,
        public_inputs: &LeaderPublic,
    ) -> bool {
        let (output_tx, output_rx) = oneshot::channel();
        // clone to send
        let utxo = *utxo;
        let public_inputs = *public_inputs;
        let () = self
            .execute(
                key_id,
                KeyOperators::Zk(Box::new(CheckLotteryWinning::new(
                    output_tx,
                    utxo,
                    public_inputs,
                ))),
            )
            .await
            .expect("KMS API should be invoked");
        output_rx.await.expect("KMS API should respond")
    }

    async fn build_private_inputs_for_winning_utxo_and_slot(
        &self,
        key_id: Self::KeyId,
        utxo: &Utxo,
        epoch_state: &EpochState,
        public_inputs: LeaderPublic,
        latest_tree: &UtxoTree,
    ) -> Result<(LeaderPrivate, Ed25519Key), PrivateInputsError> {
        // clone to send
        let utxo = *utxo;
        let epoch_state = epoch_state.clone();
        let latest_tree = latest_tree.clone();
        let (operator, output_rx, key) =
            operator_for_private_inputs_arguments_for_winning_utxo_and_slot(
                &utxo,
                &epoch_state,
                public_inputs,
                &latest_tree,
            )?;
        let () = self
            .execute(key_id, KeyOperators::Zk(Box::new(operator)))
            .await
            .expect("KMS API should be invoked");
        Ok((output_rx.await.expect("KMS API should respond"), key))
    }
}
