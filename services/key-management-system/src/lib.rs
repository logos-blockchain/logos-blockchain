use std::{
    fmt::{Debug, Display},
    sync::Arc,
};

pub use lb_key_management_system_keys::keys;
use lb_key_management_system_keys::keys::secured_key::SecuredKey;
pub use lb_key_management_system_operators as operators;
use lb_log_targets::kms;
use lb_utils::tokio::task::spawn_blocking;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        resources::ServiceResourcesHandle,
        state::{NoOperator, NoState},
    },
};
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    backend::KMSBackend,
    message::{KMSMessage, KMSSigningStrategy, KeyDescriptor},
};

pub mod api;
pub mod backend;
pub mod message;
mod metrics;

const LOG_TARGET: &str = kms::SERVICE;

pub struct KMSService<Backend, RuntimeServiceId>
where
    Backend: KMSBackend + 'static,
    Backend::KeyId: Debug,
    Backend::Key: Debug,
    Backend::Settings: Clone,
{
    backend: Backend,
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<Backend, RuntimeServiceId> ServiceData for KMSService<Backend, RuntimeServiceId>
where
    Backend: KMSBackend + 'static,
    Backend::KeyId: Debug,
    Backend::Key: Debug,
    Backend::Settings: Clone,
{
    type Settings = Backend::Settings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = KMSMessage<Backend>;
}

#[async_trait::async_trait]
impl<Backend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for KMSService<Backend, RuntimeServiceId>
where
    Backend: KMSBackend + Clone + Send + Sync + 'static,
    Backend::KeyId: Clone + Debug + Send + 'static,
    Backend::Key: Debug + Send,
    <Backend::Key as SecuredKey>::Payload: Send + 'static,
    <Backend::Key as SecuredKey>::Signature: Send + 'static,
    <Backend::Key as SecuredKey>::PublicKey: Send,
    Backend::KeyOperations: Send,
    Backend::Settings: Clone + Send + Sync,
    Backend::Error: Debug + Send + 'static,
    RuntimeServiceId: AsServiceId<Self> + Display + Send,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let backend_settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();
        let backend = Backend::new(backend_settings);
        Ok(Self {
            backend,
            service_resources_handle,
        })
    }

    async fn run(mut self) -> Result<(), DynError> {
        let Self {
            service_resources_handle:
                ServiceResourcesHandle {
                    ref mut inbound_relay,
                    status_updater,
                    ..
                },
            backend,
        } = self;

        status_updater.notify_ready();
        info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        let mut backend = Arc::new(backend);
        while let Some(msg) = inbound_relay.recv().await {
            Self::handle_kms_message(msg, &mut backend).await;
        }

        Ok(())
    }
}

impl<Backend, RuntimeServiceId> KMSService<Backend, RuntimeServiceId>
where
    Backend: KMSBackend + Clone + Send + Sync + 'static,
    Backend::KeyId: Debug + Clone + Send + 'static,
    Backend::Key: Debug,
    <Backend::Key as SecuredKey>::Payload: Send + 'static,
    <Backend::Key as SecuredKey>::Signature: Send + 'static,
    Backend::Settings: Clone,
    Backend::Error: Debug + Send + 'static,
{
    async fn handle_kms_message(message: KMSMessage<Backend>, backend: &mut Arc<Backend>) {
        match message {
            KMSMessage::Register {
                key_id,
                key_type,
                reply_channel,
            } => {
                // Signatures in flight keep the keys they started with.
                Self::handle_register(&key_id, key_type, reply_channel, Arc::make_mut(backend));
            }
            KMSMessage::PublicKey {
                key_id,
                reply_channel,
            } => Self::handle_public_key(&key_id, reply_channel, backend),
            KMSMessage::Sign {
                signing_strategy,
                payload,
                reply_channel,
            } => {
                let backend = Arc::clone(backend);
                // A ZK signature is a Groth16 proof, so it can't hold up the service.
                spawn_blocking("logos/kms/sign-blocking", move || {
                    Self::handle_sign(signing_strategy, payload, reply_channel, &backend);
                });
            }
            KMSMessage::Execute {
                key_id,
                operator,
                reply_channel,
            } => Self::handle_execute(key_id, operator, reply_channel, backend).await,
        }
    }

    fn handle_register(
        key_id: &Backend::KeyId,
        key: Backend::Key,
        reply_channel: oneshot::Sender<Result<KeyDescriptor<Backend>, Backend::Error>>,
        backend: &mut Backend,
    ) {
        metrics::kms_register_requests();

        if let Err(e) = backend.register(key_id, key) {
            metrics::kms_register_failures();

            if reply_channel.send(Err(e)).is_err() {
                debug!(
                    target: LOG_TARGET,
                    "Could not send backend key registration error to caller."
                );
            }
            return;
        }

        let pk_bytes_result = backend.public_key(key_id).map(|pk| (key_id.clone(), pk));
        if reply_channel.send(pk_bytes_result).is_err() {
            debug!(
                target: LOG_TARGET,
                "Could not reply to the public key request channel"
            );
        } else {
            metrics::kms_register_success();
        }
    }

    fn handle_public_key(
        key_id: &Backend::KeyId,
        reply_channel: oneshot::Sender<
            Result<<Backend::Key as SecuredKey>::PublicKey, Backend::Error>,
        >,
        backend: &Backend,
    ) {
        metrics::kms_public_key_requests();

        let pk_bytes_result = backend.public_key(key_id);
        if reply_channel.send(pk_bytes_result).is_err() {
            debug!(
                target: LOG_TARGET,
                "Could not reply to the public key request channel"
            );
        }
    }

    fn handle_sign(
        signing_strategy: KMSSigningStrategy<Backend::KeyId>,
        payload: <Backend::Key as SecuredKey>::Payload,
        reply_channel: oneshot::Sender<
            Result<<Backend::Key as SecuredKey>::Signature, Backend::Error>,
        >,
        backend: &Backend,
    ) {
        let signature_result = match signing_strategy {
            KMSSigningStrategy::Single(key) => {
                metrics::kms_sign_requests_single();
                let signature_result = backend.sign(&key, payload);
                metrics::kms_sign_single_result(&signature_result);
                signature_result
            }
            KMSSigningStrategy::Multi(keys) => {
                metrics::kms_sign_requests_multi();
                let signature_result = backend.sign_multiple(keys.as_slice(), payload);
                metrics::kms_sign_multi_result(&signature_result);
                signature_result
            }
        };
        if reply_channel.send(signature_result).is_err() {
            debug!(
                target: LOG_TARGET,
                "Could not reply to the public key request channel"
            );
        }
    }

    async fn handle_execute(
        key_id: Backend::KeyId,
        operator: Backend::KeyOperations,
        reply_channel: oneshot::Sender<Result<(), Backend::Error>>,
        backend: &Backend,
    ) {
        metrics::kms_execute_requests();
        let result = backend.execute(&key_id, operator).await;
        if let Err(e) = &result {
            metrics::kms_execute_failures();
            error!(
                target: LOG_TARGET,
                "Failed to execute operator with key ID {key_id:?}. Error: {e:?}"
            );
        }

        if reply_channel.send(result).is_err() {
            debug!(
                target: LOG_TARGET,
                "Could not reply to the execute request channel"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Mutex, mpsc},
        time::Duration,
    };

    use bytes::Bytes;
    use lb_key_management_system_keys::keys::{
        Ed25519Key, Key, KeyOperators, PayloadEncoding, PublicKeyEncoding, SignatureEncoding,
    };
    use rand::rngs::OsRng;

    use super::*;
    use crate::backend::preload::{
        KeyId, PreloadBackendError, PreloadKMSBackend, PreloadKMSBackendSettings,
    };

    /// Fails a signature the test never released, rather than hanging when
    /// the signature holds up the test itself.
    const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

    /// Holds every signature back until the test releases it.
    #[derive(Clone)]
    struct StalledSigningBackend {
        keys: PreloadKMSBackend,
        release: Arc<Mutex<mpsc::Receiver<()>>>,
    }

    #[async_trait::async_trait]
    impl KMSBackend for StalledSigningBackend {
        type KeyId = KeyId;
        type Key = Key;
        type KeyOperations = KeyOperators;
        type Settings = (PreloadKMSBackendSettings, Arc<Mutex<mpsc::Receiver<()>>>);
        type Error = PreloadBackendError;

        fn new((settings, release): Self::Settings) -> Self {
            Self {
                keys: PreloadKMSBackend::new(settings),
                release,
            }
        }

        fn register(&mut self, key_id: &KeyId, key: Key) -> Result<(), Self::Error> {
            self.keys.register(key_id, key)
        }

        fn public_key(&self, key_id: &KeyId) -> Result<PublicKeyEncoding, Self::Error> {
            self.keys.public_key(key_id)
        }

        fn sign(
            &self,
            key_id: &KeyId,
            payload: PayloadEncoding,
        ) -> Result<SignatureEncoding, Self::Error> {
            self.release
                .lock()
                .unwrap()
                .recv_timeout(RELEASE_TIMEOUT)
                .expect("The signature should be released by the test.");
            self.keys.sign(key_id, payload)
        }

        fn sign_multiple(
            &self,
            key_ids: &[KeyId],
            payload: PayloadEncoding,
        ) -> Result<SignatureEncoding, Self::Error> {
            self.keys.sign_multiple(key_ids, payload)
        }

        async fn execute(&self, key_id: &KeyId, operator: KeyOperators) -> Result<(), Self::Error> {
            self.keys.execute(key_id, operator).await
        }
    }

    type Service = KMSService<StalledSigningBackend, ()>;

    #[tokio::test]
    async fn a_signature_in_flight_leaves_the_service_free() {
        let key_id = "key".to_owned();
        let key = Key::Ed25519(Ed25519Key::generate(&mut OsRng));
        let (release, stalled) = mpsc::channel();
        let mut backend = Arc::new(StalledSigningBackend::new((
            PreloadKMSBackendSettings {
                keys: HashMap::from_iter([(key_id.clone(), key.clone())]),
            },
            Arc::new(Mutex::new(stalled)),
        )));

        let (reply_channel, signature) = oneshot::channel();
        Service::handle_kms_message(
            KMSMessage::Sign {
                signing_strategy: KMSSigningStrategy::Single(key_id.clone()),
                payload: PayloadEncoding::Ed25519(Bytes::from("data")),
                reply_channel,
            },
            &mut backend,
        )
        .await;

        let (reply_channel, public_key) = oneshot::channel();
        Service::handle_kms_message(
            KMSMessage::PublicKey {
                key_id: key_id.clone(),
                reply_channel,
            },
            &mut backend,
        )
        .await;
        assert_eq!(public_key.await.unwrap().unwrap(), key.as_public_key());

        let (reply_channel, registered) = oneshot::channel();
        Service::handle_kms_message(
            KMSMessage::Register {
                key_id: "registered during the signature".to_owned(),
                key_type: key.clone(),
                reply_channel,
            },
            &mut backend,
        )
        .await;
        assert!(registered.await.unwrap().is_ok());

        release.send(()).unwrap();
        assert!(signature.await.unwrap().is_ok());
    }
}
