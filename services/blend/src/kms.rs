use core::fmt::{Debug, Display};

use lb_key_management_system_service::{
    KMSService,
    api::KmsServiceApi,
    backend::preload::{KeyId, PreloadKMSBackend},
    keys::{KeyOperators, ZkPublicKey},
    operators::blend::zk_id::ZkIdOperator,
};
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;

pub type PreloadKmsService<RuntimeServiceId> = KMSService<PreloadKMSBackend, RuntimeServiceId>;

/// The `zk_id` of the quota secret key `key_id`, `zkhash(KDF, core_sk)`, which
/// the Proof of Quota proves. The KMS public key of the same key is the public
/// key of its notes, derived from its nullifier key.
pub async fn zk_id<RuntimeServiceId>(
    kms_api: &KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
    key_id: KeyId,
) -> ZkPublicKey
where
    RuntimeServiceId:
        AsServiceId<PreloadKmsService<RuntimeServiceId>> + Debug + Display + Send + Sync + 'static,
{
    let (sender, receiver) = oneshot::channel();
    Box::pin(kms_api.execute(
        key_id,
        KeyOperators::Zk(Box::new(ZkIdOperator::new(sender))),
    ))
    .await
    .expect("ZK key for provided ID should be stored in KMS.");
    receiver
        .await
        .expect("KMS should derive the zk_id of a ZK key.")
}
