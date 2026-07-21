use std::{fmt::Display, marker::PhantomData};

use bytes::Bytes;
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
#[cfg(feature = "rocksdb-backend")]
use lb_services_utils::overwatch::recovery::RecoveryData;
pub use lb_services_utils::overwatch::recovery::StorageRecoverySettings;
use lb_services_utils::overwatch::recovery::{RecoveryBackend, RecoveryError, RecoveryResult};
#[cfg(feature = "rocksdb-backend")]
use overwatch::DynError;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, relay::OutboundRelay, state::ServiceState},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::OnceCell;

#[cfg(feature = "rocksdb-backend")]
use crate::backends::rocksdb::{RocksBackend, RocksBackendSettings};
use crate::{StorageMsg, StorageService, backends::StorageBackend};

const RECOVERY_PREFIX: &[u8] = b"recovery/";

#[must_use]
pub fn recovery_key(suffix: &[u8]) -> Bytes {
    let mut key = Vec::with_capacity(RECOVERY_PREFIX.len() + suffix.len());
    key.extend_from_slice(RECOVERY_PREFIX);
    key.extend_from_slice(suffix);
    key.into()
}

#[cfg(feature = "rocksdb-backend")]
pub fn load_recovery_data(settings: RocksBackendSettings) -> Result<RecoveryData, DynError> {
    let backend = RocksBackend::new(settings)?;
    recovery_data_from_backend(&backend)
}

#[cfg(feature = "rocksdb-backend")]
fn recovery_data_from_backend(backend: &RocksBackend) -> Result<RecoveryData, DynError> {
    backend
        .load_prefix_entries(RECOVERY_PREFIX)
        .map(RecoveryData::new)
        .map_err(Into::into)
}

pub struct StorageRecoveryBackend<State, Settings, Storage: StorageBackend, RuntimeServiceId> {
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    storage_relay: OnceCell<OutboundRelay<StorageMsg<Storage>>>,
    state: PhantomData<fn() -> State>,
    settings: PhantomData<fn() -> Settings>,
}

impl<State, Settings, Storage, RuntimeServiceId> Clone
    for StorageRecoveryBackend<State, Settings, Storage, RuntimeServiceId>
where
    Storage: StorageBackend,
    OverwatchHandle<RuntimeServiceId>: Clone,
{
    fn clone(&self) -> Self {
        Self {
            overwatch_handle: self.overwatch_handle.clone(),
            storage_relay: self.storage_relay.clone(),
            state: PhantomData,
            settings: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<State, Settings, Storage, RuntimeServiceId> RecoveryBackend<RuntimeServiceId>
    for StorageRecoveryBackend<State, Settings, Storage, RuntimeServiceId>
where
    State: ServiceState<Settings = Settings> + Serialize + DeserializeOwned + Send,
    Settings: StorageRecoverySettings + Send,
    Storage: StorageBackend + Send + Sync + 'static,
    RuntimeServiceId: Clone
        + std::fmt::Debug
        + Display
        + Send
        + Sync
        + 'static
        + AsServiceId<StorageService<Storage, RuntimeServiceId>>,
{
    type State = State;

    fn from_settings(
        _settings: &Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        Self {
            overwatch_handle,
            storage_relay: OnceCell::new(),
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
        let storage_relay = self
            .storage_relay
            .get_or_try_init(async || {
                self.overwatch_handle
                    .relay::<StorageService<Storage, RuntimeServiceId>>()
                    .await
                    .map_err(|error| RecoveryError::Backend(error.to_string()))
            })
            .await?;

        let message = StorageMsg::Store {
            key: recovery_key(Settings::RECOVERY_KEY_SUFFIX),
            value: state
                .to_bytes()
                .map_err(|error| RecoveryError::Backend(error.to_string()))?,
        };

        storage_relay
            .send(message)
            .await
            .map_err(|(error, _message)| RecoveryError::Backend(error.to_string()))
    }
}

#[cfg(all(test, feature = "rocksdb-backend"))]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    type TestBackend =
        StorageRecoveryBackend<TestState, TestSettings, RocksBackend, TestRuntimeServiceId>;

    #[derive(Clone, Debug)]
    enum TestRuntimeServiceId {
        Storage,
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "{self:?}")
        }
    }

    impl AsServiceId<StorageService<RocksBackend, Self>> for TestRuntimeServiceId {
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
