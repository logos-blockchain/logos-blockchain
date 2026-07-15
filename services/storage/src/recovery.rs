use std::{fmt::Display, marker::PhantomData};

use bytes::Bytes;
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
use lb_services_utils::overwatch::recovery::{
    RecoveryBackend, RecoveryError, RecoveryReader, RecoveryResult,
};
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, relay::OutboundRelay, state::ServiceState},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::OnceCell;

use crate::{StorageMsg, StorageService, backends::StorageBackend};

pub trait StorageRecoverySettings {
    const RECOVERY_KEY: &'static [u8];

    fn recovery_reader(&self) -> Option<&RecoveryReader>;
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
        let Some(reader) = settings.recovery_reader() else {
            return Ok(None);
        };

        let Some(bytes) = reader.read(Settings::RECOVERY_KEY)? else {
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
            key: Bytes::from_static(Settings::RECOVERY_KEY),
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
    use overwatch::DynError;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::backends::rocksdb::RocksBackend;

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
        recovery_reader: Option<RecoveryReader>,
    }

    impl StorageRecoverySettings for TestSettings {
        const RECOVERY_KEY: &'static [u8] = b"recovery/test";

        fn recovery_reader(&self) -> Option<&RecoveryReader> {
            self.recovery_reader.as_ref()
        }
    }

    #[test]
    fn loads_state_from_configured_key() {
        let expected = TestState {
            value: "restored".into(),
        };
        let directory = tempfile::tempdir().unwrap();
        let reader = RocksBackend::new(crate::backends::rocksdb::RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        let bytes = expected.to_bytes().unwrap();
        reader
            .txn(move |database| {
                database.put(TestSettings::RECOVERY_KEY, bytes)?;
                Ok(None)
            })
            .execute()
            .unwrap();
        let recovery_reader = reader.into_recovery_reader();
        let settings = TestSettings {
            recovery_reader: Some(recovery_reader),
        };

        let state = <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
            .unwrap()
            .unwrap();

        assert_eq!(state, expected);
        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings).is_err()
        );
    }

    #[test]
    fn missing_recovery_state_returns_none() {
        let settings = TestSettings {
            recovery_reader: None,
        };

        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn missing_recovery_key_returns_none_and_releases_reader() {
        let directory = tempfile::tempdir().unwrap();
        let recovery_reader = RocksBackend::new(crate::backends::rocksdb::RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        })
        .unwrap()
        .into_recovery_reader();
        let settings = TestSettings {
            recovery_reader: Some(recovery_reader),
        };

        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings)
                .unwrap()
                .is_none()
        );
        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings).is_err()
        );
    }

    #[test]
    fn invalid_recovery_state_consumes_reader() {
        let directory = tempfile::tempdir().unwrap();
        let reader = RocksBackend::new(crate::backends::rocksdb::RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        reader
            .txn(|database| {
                database.put(TestSettings::RECOVERY_KEY, b"invalid recovery state")?;
                Ok(None)
            })
            .execute()
            .unwrap();
        let recovery_reader = reader.into_recovery_reader();
        let settings = TestSettings {
            recovery_reader: Some(recovery_reader),
        };

        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings).is_err()
        );
        assert!(
            <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(&settings).is_err()
        );
    }
}
