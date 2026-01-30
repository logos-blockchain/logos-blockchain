use std::fmt::{Debug, Display};

use lb_key_management_system_service::{
    KMSService,
    api::KmsServiceApi,
    backend::preload::{KeyId, PreloadKMSBackend},
    keys::{KeyOperators, UnsecuredZkKey},
    operators::zk::leader::unsafe_leader_key_operator::UnsafeLeaderKeyOperator,
};
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;

pub type PreloadKmsService<RuntimeServiceId> = KMSService<PreloadKMSBackend, RuntimeServiceId>;

#[async_trait::async_trait]
pub trait KmsAdapter<RuntimeServiceId> {
    type KeyId;

    async fn get_leader_key(&self, key_id: Self::KeyId) -> UnsecuredZkKey;
}

#[async_trait::async_trait]
impl<RuntimeServiceId> KmsAdapter<RuntimeServiceId>
    for KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>
where
    RuntimeServiceId:
        AsServiceId<PreloadKmsService<RuntimeServiceId>> + Debug + Display + Send + Sync + 'static,
{
    type KeyId = KeyId;

    async fn get_leader_key(&self, key_id: Self::KeyId) -> UnsecuredZkKey {
        let (output_tx, output_rx) = oneshot::channel();
        let () = self
            .execute(
                key_id,
                KeyOperators::Zk(Box::new(UnsafeLeaderKeyOperator::new(output_tx))),
            )
            .await
            .expect("KMS API should be invoked");
        output_rx.await.expect("KMS API should respond")
    }
}
