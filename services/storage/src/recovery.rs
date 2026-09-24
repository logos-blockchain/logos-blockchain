use std::{fmt::Display, marker::PhantomData};

use bytes::Bytes;
use lb_binary_codec::bincode::DeserializeOp as _;
#[cfg(test)]
use lb_binary_codec::bincode::SerializeOp as _;
pub use lb_services_utils::overwatch::recovery::StorageRecoverySettings;
use lb_services_utils::overwatch::recovery::{
    RecoveryBackend, RecoveryData, RecoveryError, RecoveryResult,
};
use overwatch::{
    DynError,
    overwatch::OverwatchHandle,
    services::{AsServiceId, state::ServiceState},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::OnceCell;

use crate::{
    StorageService,
    api::StorageApi,
    backend::StorageBackend as _,
    rocksdb::{RocksBackend, RocksBackendSettings},
};

const RECOVERY_PREFIX: &[u8] = b"recovery/";

#[must_use]
pub fn recovery_key(suffix: &[u8]) -> Bytes {
    let mut key = Vec::with_capacity(RECOVERY_PREFIX.len() + suffix.len());
    key.extend_from_slice(RECOVERY_PREFIX);
    key.extend_from_slice(suffix);
    key.into()
}

pub fn load_recovery_data(settings: RocksBackendSettings) -> Result<RecoveryData, DynError> {
    let backend = RocksBackend::new(settings)?;
    recovery_data_from_backend(&backend)
}

fn recovery_data_from_backend(backend: &RocksBackend) -> Result<RecoveryData, DynError> {
    backend
        .load_prefix_entries(RECOVERY_PREFIX)
        .map(RecoveryData::new)
        .map_err(Into::into)
}

pub struct StorageRecoveryBackend<State, Settings, RuntimeServiceId> {
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    storage: OnceCell<StorageApi>,
    state: PhantomData<fn() -> State>,
    settings: PhantomData<fn() -> Settings>,
}

impl<State, Settings, RuntimeServiceId> Clone
    for StorageRecoveryBackend<State, Settings, RuntimeServiceId>
where
    OverwatchHandle<RuntimeServiceId>: Clone,
{
    fn clone(&self) -> Self {
        Self {
            overwatch_handle: self.overwatch_handle.clone(),
            storage: self.storage.clone(),
            state: PhantomData,
            settings: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<State, Settings, RuntimeServiceId> RecoveryBackend<RuntimeServiceId>
    for StorageRecoveryBackend<State, Settings, RuntimeServiceId>
where
    State: ServiceState<Settings = Settings> + Serialize + DeserializeOwned + Send,
    Settings: StorageRecoverySettings + Send,
    RuntimeServiceId: Clone
        + std::fmt::Debug
        + Display
        + Send
        + Sync
        + 'static
        + AsServiceId<StorageService<RuntimeServiceId>>,
{
    type State = State;

    fn from_settings(
        _settings: &Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        Self {
            overwatch_handle,
            storage: OnceCell::new(),
            state: PhantomData,
            settings: PhantomData,
        }
    }

    fn load_state(settings: &Settings) -> RecoveryResult<Option<Self::State>> {
        let Some(bytes) = settings
            .recovery_data()
            .take(&recovery_key(Settings::RECOVERY_KEY_SUFFIX))?
        else {
            return Ok(None);
        };

        State::from_bytes(&bytes)
            .map(Some)
            .map_err(|error| RecoveryError::Backend(error.to_string()))
    }

    async fn save_state(&mut self, state: Self::State) -> RecoveryResult<()> {
        let storage = self
            .storage
            .get_or_try_init(async || {
                StorageApi::from_overwatch_handle(&self.overwatch_handle)
                    .await
                    .map_err(|error| RecoveryError::Backend(error.to_string()))
            })
            .await?;

        storage
            .store(recovery_key(Settings::RECOVERY_KEY_SUFFIX), state)
            .await
            .map_err(|error| RecoveryError::Backend(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    type TestBackend = StorageRecoveryBackend<TestState, TestSettings, TestRuntimeServiceId>;

    #[derive(Clone, Debug)]
    enum TestRuntimeServiceId {
        Storage,
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "{self:?}")
        }
    }

    impl AsServiceId<StorageService<Self>> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self::Storage;
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TestState {
        value: String,
    }

    impl ServiceState for TestState {
        type Settings = TestSettings;
        type Error = DynError;

        fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
            Ok(Self {
                value: String::new(),
            })
        }
    }

    #[derive(Clone, Debug)]
    struct TestSettings {
        recovery_data: RecoveryData,
    }

    impl StorageRecoverySettings for TestSettings {
        const RECOVERY_KEY_SUFFIX: &'static [u8] = b"test";

        fn recovery_data(&self) -> &RecoveryData {
            &self.recovery_data
        }
    }

    #[test]
    fn loads_and_removes_state_from_configured_key() {
        let expected = TestState {
            value: "restored".into(),
        };
        let directory = tempfile::tempdir().unwrap();
        let reader = RocksBackend::new(RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        let bytes = expected.to_bytes().unwrap();
        reader
            .txn(move |database| {
                database.put(recovery_key(TestSettings::RECOVERY_KEY_SUFFIX), bytes)?;
                Ok(None)
            })
            .execute()
            .unwrap();
        let recovery_data = recovery_data_from_backend(&reader).unwrap();
        let settings = TestSettings { recovery_data };

        let state = <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
            .unwrap()
            .unwrap();

        assert_eq!(state, expected);
        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn missing_recovery_state_returns_none() {
        let settings = TestSettings {
            recovery_data: RecoveryData::default(),
        };

        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn missing_recovery_key_returns_none() {
        let directory = tempfile::tempdir().unwrap();
        let backend = RocksBackend::new(RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        let recovery_data = recovery_data_from_backend(&backend).unwrap();
        let settings = TestSettings { recovery_data };

        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
                .unwrap()
                .is_none()
        );
        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_recovery_state_remains_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let reader = RocksBackend::new(RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        reader
            .txn(|database| {
                database.put(
                    recovery_key(TestSettings::RECOVERY_KEY_SUFFIX),
                    b"invalid recovery state",
                )?;
                Ok(None)
            })
            .execute()
            .unwrap();
        let recovery_data = recovery_data_from_backend(&reader).unwrap();
        let settings = TestSettings { recovery_data };

        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings).is_err()
        );
        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
                .unwrap()
                .is_none()
        );
    }
}
