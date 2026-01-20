pub mod backends;
pub mod errors;
pub mod operators;
pub mod serializer;

pub use backends::{FileBackend, JsonFileBackend};
pub use errors::RecoveryError;
pub use operators::RecoveryOperator;
pub use serializer::JsonRecoverySerializer;

pub type RecoveryResult<T> = Result<T, RecoveryError>;

#[cfg(test)]
mod tests {
    use std::{env::temp_dir, path::PathBuf};

    use async_trait::async_trait;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle, derive_services,
        overwatch::OverwatchRunner,
        services::{AsServiceId, ServiceCore, ServiceData, state::ServiceState},
    };
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::{overwatch::recovery::backends::FileBackendSettings, traits::FromSettings as _};

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct MyState {
        value: String,
    }

    impl ServiceState for MyState {
        type Settings = SettingsWithRecovery;
        type Error = DynError;
        fn from_settings(_settings: &Self::Settings) -> Result<Self, DynError> {
            Ok(Self::default())
        }
    }

    #[derive(Debug)]
    pub enum MyMessage {}

    #[derive(Debug, Clone)]
    pub struct SettingsWithRecovery {
        recovery_file: PathBuf,
    }

    impl FileBackendSettings for SettingsWithRecovery {
        fn recovery_file(&self) -> &PathBuf {
            &self.recovery_file
        }
    }

    struct Recovery {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for Recovery {
        type Settings = SettingsWithRecovery;
        type State = MyState;
        type StateOperator = RecoveryOperator<JsonFileBackend<Self::State, Self::Settings>>;
        type Message = MyMessage;
    }

    #[async_trait]
    impl ServiceCore<RuntimeServiceId> for Recovery {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            initial_state: Self::State,
        ) -> Result<Self, DynError> {
            assert_eq!(initial_state.value, "");
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            let Self {
                service_resources_handle,
            } = self;

            service_resources_handle.status_updater.notify_ready();
            tracing::info!(
                "Service '{}' is ready.",
                <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
            );

            service_resources_handle
                .state_updater
                .update(Some(Self::State {
                    value: "Hello".to_owned(),
                }));

            drop(service_resources_handle.overwatch_handle.shutdown().await);
            Ok(())
        }
    }

    #[derive_services]
    pub struct RecoveryTest {
        recovery: Recovery,
    }

    #[test]
    fn test_recovery() {
        // Initialize recovery file backend
        let recovery_file = temp_dir().join("recovery_test.json");
        let recovery_settings = SettingsWithRecovery { recovery_file };
        let file_backend =
            JsonFileBackend::<MyState, SettingsWithRecovery>::from_settings(&recovery_settings);

        // Run the service with recovery enabled
        let service_settings = RecoveryTestServiceSettings {
            recovery: recovery_settings,
        };
        let app = OverwatchRunner::<RecoveryTest>::run(service_settings, None).unwrap();
        drop(
            app.runtime()
                .handle()
                .block_on(app.handle().start_all_services()),
        );
        app.blocking_wait_finished();

        // Read the content of the recovery file
        let serialized_state = std::fs::read_to_string(file_backend.recovery_file());

        // Early cleanup (to avoid left over due to test failure)
        std::fs::remove_file(file_backend.recovery_file()).unwrap();

        // Verify the recovery file was created and contains the correct state
        assert!(serialized_state.is_ok());
        assert_eq!(serialized_state.unwrap(), "{\"value\":\"Hello\"}");
    }
}
