use std::fmt::{Debug, Display};

use lb_core::{mantle::Utxo, proofs::leader_proof::LeaderPublic};
use lb_key_management_system_service::{
    KMSService,
    api::KmsServiceApi,
    backend::preload::{KeyId, PreloadKMSBackend},
    keys::{KeyOperators, UnsecuredZkKey},
    operators::zk::leader::CheckConditionWithLeaderKey,
};
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;

use crate::leadership::check_winning;

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
        let utxo = utxo.clone();
        let public_inputs = public_inputs.clone();
        let () = self
            .execute(
                key_id,
                KeyOperators::Zk(Box::new(CheckConditionWithLeaderKey::new(
                    output_tx,
                    move |key: &UnsecuredZkKey| check_winning(utxo, public_inputs, key),
                ))),
            )
            .await
            .expect("KMS API should be invoked");
        output_rx.await.expect("KMS API should respond")
    }
}
