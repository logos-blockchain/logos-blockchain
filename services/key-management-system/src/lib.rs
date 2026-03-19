use std::fmt::{Debug, Display};

pub use lb_key_management_system_keys::keys;
use lb_key_management_system_keys::keys::secured_key::SecuredKey;
pub use lb_key_management_system_operators as operators;
use log::error;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};

use crate::{
    backend::KMSBackend,
    message::{KMSMessage, KMSSigningStrategy},
};

pub mod api;
pub mod backend;
pub mod message;
mod metrics;

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
    Backend: KMSBackend + Send + 'static,
    Backend::KeyId: Clone + Debug + Send,
    Backend::Key: Debug + Send,
    <Backend::Key as SecuredKey>::Payload: Send,
    <Backend::Key as SecuredKey>::Signature: Send,
    <Backend::Key as SecuredKey>::PublicKey: Send,
    Backend::KeyOperations: Send,
    Backend::Settings: Clone + Send + Sync,
    Backend::Error: Debug + Send,
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
                OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                    ref mut inbound_relay,
                    status_updater,
                    ..
                },
            mut backend,
        } = self;

        status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        while let Some(msg) = inbound_relay.recv().await {
            Self::handle_kms_message(msg, &mut backend).await;
        }

        Ok(())
    }
}

impl<Backend, RuntimeServiceId> KMSService<Backend, RuntimeServiceId>
where
    Backend: KMSBackend + 'static,
    Backend::KeyId: Debug + Clone,
    Backend::Key: Debug,
    Backend::Settings: Clone,
    Backend::Error: Debug,
{
    async fn handle_kms_message(message: KMSMessage<Backend>, backend: &mut Backend) {
        match message {
            KMSMessage::Register {
                key_id,
                key_type,
                reply_channel,
            } => {
                metrics::kms_register_requests();

                if let Err(e) = backend.register(&key_id, key_type) {
                    metrics::kms_register_failures();

                    if reply_channel.send(Err(e)).is_err() {
                        error!("Could not send backend key registration error to caller.");
                    }
                    return;
                }

                let pk_bytes_result = backend.public_key(&key_id).map(|pk| (key_id.clone(), pk));
                if reply_channel.send(pk_bytes_result).is_err() {
                    error!("Could not reply to the public key request channel");
                } else {
                    metrics::kms_register_success();
                }
            }
            KMSMessage::PublicKey {
                key_id,
                reply_channel,
            } => {
                metrics::kms_public_key_requests();

                let pk_bytes_result = backend.public_key(&key_id);
                if reply_channel.send(pk_bytes_result).is_err() {
                    error!("Could not reply to the public key request channel");
                }
            }
            KMSMessage::Sign {
                signing_strategy,
                payload,
                reply_channel,
            } => {
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
                    error!("Could not reply to the public key request channel");
                }
            }
            KMSMessage::Execute { key_id, operator } => {
                // TODO: Bubble up errors: https://github.com/logos-blockchain/logos-blockchain/issues/2079
                metrics::kms_execute_requests();
                drop(backend.execute(&key_id, operator).await.inspect_err(|e| {
                    metrics::kms_execute_failures();
                    error!("Failed to execute operator with key ID {key_id:?}. Error: {e:?}");
                }));
            }
        }
    }
}
