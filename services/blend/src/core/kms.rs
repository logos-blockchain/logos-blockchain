use core::fmt::{Debug, Display};

use async_trait::async_trait;
use lb_blend::{
    proofs::quota::{self, VerifiedProofOfQuota, inputs::prove::PublicInputs},
    scheduling::message_blend::CoreProofOfQuotaGenerator,
};
use lb_core::crypto::ZkHash;
use lb_key_management_system_service::{
    api::KmsServiceApi, backend::preload::KeyId, keys::KeyOperators,
    operators::blend::poq::PoQOperator,
};
use lb_poq::CorePathAndSelectors;
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;

use crate::kms::PreloadKmsService;

const LOG_TARGET: &str = "blend::service::core::kms-poq-generator";

#[async_trait]
pub trait KmsPoQAdapter<RuntimeServiceId> {
    type CorePoQGenerator;
    type KeyId;

    fn core_poq_generator(
        &self,
        key_id: Self::KeyId,
        core_path_and_selectors: Box<CorePathAndSelectors>,
    ) -> Self::CorePoQGenerator;
}

pub type PreloadKmsServiceKeyId<RuntimeServiceId> = <KmsServiceApi<
    PreloadKmsService<RuntimeServiceId>,
    RuntimeServiceId,
> as KmsPoQAdapter<RuntimeServiceId>>::KeyId;

#[async_trait]
impl<RuntimeServiceId> KmsPoQAdapter<RuntimeServiceId>
    for KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>
{
    type CorePoQGenerator = PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>;
    type KeyId = KeyId;

    fn core_poq_generator(
        &self,
        key_id: Self::KeyId,
        core_path_and_selectors: Box<CorePathAndSelectors>,
    ) -> Self::CorePoQGenerator {
        tracing::debug!(target: LOG_TARGET, "Creating KMS-based PoQ generator with key ID {key_id:?} and core path and selectors {core_path_and_selectors:?}");
        PreloadKMSBackendCorePoQGenerator {
            core_path_and_selectors: *core_path_and_selectors,
            kms_api: self.clone(),
            key_id,
        }
    }
}

#[derive(Clone)]
pub struct PreloadKMSBackendCorePoQGenerator<RuntimeServiceId> {
    core_path_and_selectors: CorePathAndSelectors,
    kms_api: KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
    key_id: KeyId,
}

impl<RuntimeServiceId> CoreProofOfQuotaGenerator
    for PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>
where
    RuntimeServiceId:
        AsServiceId<PreloadKmsService<RuntimeServiceId>> + Debug + Display + Send + Sync + 'static,
{
    fn generate_poq(
        &self,
        public_inputs: &PublicInputs,
        key_index: u64,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync
    {
        tracing::debug!(target: LOG_TARGET, "Generating KMS-based PoQ with public_inputs {public_inputs:?} and key_index {key_index:?}.");

        let kms_api = self.kms_api.clone();
        let key_id = self.key_id.clone();
        let core_path_and_selectors = self.core_path_and_selectors;

        async move {
            let (res_sender, res_receiver) = oneshot::channel();

            generate_and_send_kms_poq(
                kms_api,
                key_id,
                public_inputs,
                key_index,
                &core_path_and_selectors,
                res_sender,
            )
            .await;

            let poq = res_receiver
                .await
                .expect("Should not fail to get PoQ generation result from KMS.")?;
            tracing::debug!(target: LOG_TARGET, "KMS-based PoQ generation succeeded.");
            Ok(poq)
        }
    }
}

async fn generate_and_send_kms_poq<RuntimeServiceId>(
    kms_api: KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
    key_id: KeyId,
    public_inputs: &PublicInputs,
    key_index: u64,
    core_path_and_selectors: &CorePathAndSelectors,
    result_sender: oneshot::Sender<Result<(VerifiedProofOfQuota, ZkHash), quota::Error>>,
) where
    RuntimeServiceId:
        AsServiceId<PreloadKmsService<RuntimeServiceId>> + Debug + Display + Send + Sync + 'static,
{
    let () = kms_api
        .execute(
            key_id,
            KeyOperators::Zk(Box::new(PoQOperator::new(
                *core_path_and_selectors,
                *public_inputs,
                key_index,
                result_sender,
            ))),
        )
        .await
        .expect("Execute API should run successfully.");
}
