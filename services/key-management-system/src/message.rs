use key_management_system_keys::keys::secured_key::SecuredKey;
use tokio::sync::oneshot;

use crate::backend::KMSBackend;

pub type KeyDescriptor<Backend> = (
    <Backend as KMSBackend>::KeyId,
    <<Backend as KMSBackend>::Key as SecuredKey>::PublicKey,
);

// TODO: Remove since we have an `execute` API that allows for signing.
#[derive(Debug)]
pub enum KMSSigningStrategy<KeyId> {
    Single(KeyId),
    Multi(Vec<KeyId>),
}

pub enum KMSMessage<Backend>
where
    Backend: KMSBackend,
{
    Register {
        key_id: Backend::KeyId,
        key_type: Backend::Key,
        reply_channel: oneshot::Sender<Result<KeyDescriptor<Backend>, Backend::Error>>,
    },
    PublicKey {
        key_id: Backend::KeyId,
        reply_channel:
            oneshot::Sender<Result<<Backend::Key as SecuredKey>::PublicKey, Backend::Error>>,
    },
    Sign {
        signing_strategy: KMSSigningStrategy<Backend::KeyId>,
        payload: <Backend::Key as SecuredKey>::Payload,
        reply_channel:
            oneshot::Sender<Result<<Backend::Key as SecuredKey>::Signature, Backend::Error>>,
    },
    Execute {
        key_id: Backend::KeyId,
        operator: Backend::KeyOperations,
    },
}
