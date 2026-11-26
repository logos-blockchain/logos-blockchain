use std::{fmt::Display, marker::PhantomData, time::Duration};

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
    services::{
        AsServiceId,
        state::ServiceState,
        status::{ServiceStatus, StatusWatcher, WaitPolicy},
    },
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{sync::OnceCell, time::timeout};

use crate::{
    StorageService,
    api::StorageApi,
    backend::StorageBackend as _,
    rocksdb::{RocksBackend, RocksBackendSettings},
};

const RECOVERY_PREFIX: &[u8] = b"recovery/";

// Overwatch may already be waiting for this save when it receives the lookup.
const OVERWATCH_ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

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
    storage: OnceCell<(StorageApi, StatusWatcher)>,
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
        let (storage, status) = self
            .storage
            .get_or_try_init(async || {
                timeout(OVERWATCH_ANSWER_TIMEOUT, async {
                    tokio::try_join!(
                        StorageApi::from_overwatch_handle(&self.overwatch_handle),
                        self.overwatch_handle.status_watcher::<StorageService<RuntimeServiceId>>(),
                    )
                })
                .await
                .map_err(|_| {
                    RecoveryError::Backend(format!(
                        "Overwatch did not supply the storage relay and status within {OVERWATCH_ANSWER_TIMEOUT:?}"
                    ))
                })?
                .map_err(|error| RecoveryError::Backend(error.to_string()))
            })
            .await?;

        let mut status = status.clone();

        // When capacity is available, storage can process the queued save after
        // it restarts. A stopped service cannot drain a full relay.
        tokio::select! {
            biased;
            result = storage.store(recovery_key(Settings::RECOVERY_KEY_SUFFIX), state) => {
                result.map_err(|error| RecoveryError::Backend(error.to_string()))
            }
            stopped = status.wait_with_policy(ServiceStatus::Stopped, None, WaitPolicy::FailFast) => {
                match stopped {
                    Ok(_) => Err(RecoveryError::NotRunning("storage stopped during the recovery save".into())),
                    Err(error) => Err(RecoveryError::Backend(error.to_string())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;

    use futures::poll;
    use overwatch::services::relay::OutboundRelay;
    use serde::{Deserialize, Serialize};
    use tokio::{
        runtime::Handle,
        sync::{mpsc, watch},
    };

    use super::*;
    use crate::StorageMsg;

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

    #[tokio::test(start_paused = true)]
    async fn recovery_save_times_out_when_overwatch_does_not_answer() {
        let (commands, requests) = mpsc::channel(2);
        let overwatch = OverwatchHandle::new(Handle::current(), commands);
        let settings = TestSettings {
            recovery_data: RecoveryData::default(),
        };
        let mut backend = TestBackend::from_settings(&settings, overwatch);
        let mut save = pin!(backend.save_state(TestState {
            value: "snapshot".into(),
        }));

        assert!(poll!(save.as_mut()).is_pending());

        // Keep the queued lookup requests alive without answering them.
        let result = timeout(OVERWATCH_ANSWER_TIMEOUT + Duration::from_secs(2), save)
            .await
            .expect("the save should give up on the unanswered Overwatch requests");
        assert_eq!(requests.len(), 2);
        assert!(
            matches!(result, Err(RecoveryError::Backend(message)) if message.contains("Overwatch did not supply"))
        );
    }

    #[tokio::test]
    async fn recovery_save_cancels_when_storage_stops_with_a_full_relay() {
        let (commands, _requests) = mpsc::channel(1);
        let overwatch = OverwatchHandle::new(Handle::current(), commands);
        let settings = TestSettings {
            recovery_data: RecoveryData::default(),
        };
        let mut backend = TestBackend::from_settings(&settings, overwatch);

        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(StorageMsg::Store {
                key: Bytes::from_static(b"filler"),
                value: Bytes::new(),
            })
            .unwrap();

        let (status, watcher) = watch::channel(ServiceStatus::Ready);
        let storage = StorageApi::new(OutboundRelay::new(sender));
        assert!(
            backend
                .storage
                .set((storage, StatusWatcher::new(watcher)))
                .is_ok()
        );

        let result = {
            let mut save = pin!(backend.save_state(TestState {
                value: "snapshot".into(),
            }));
            assert!(poll!(save.as_mut()).is_pending());

            status.send(ServiceStatus::Stopped).unwrap();
            timeout(Duration::from_secs(1), save)
                .await
                .expect("shutdown should release a save blocked on the full storage relay")
        };
        assert!(matches!(result, Err(RecoveryError::NotRunning(_))));

        status.send(ServiceStatus::Failed).unwrap();
        let result = timeout(
            Duration::from_secs(1),
            backend.save_state(TestState {
                value: "snapshot after failure".into(),
            }),
        )
        .await
        .expect("the save should return an error when storage has failed");
        assert!(
            matches!(result, Err(RecoveryError::Backend(message)) if message.contains("service failed"))
        );

        assert!(
            matches!(receiver.try_recv().unwrap(), StorageMsg::Store { key, .. } if key == b"filler"[..])
        );

        status.send(ServiceStatus::Ready).unwrap();
        timeout(
            Duration::from_secs(1),
            backend.save_state(TestState {
                value: "next snapshot".into(),
            }),
        )
        .await
        .expect("the next save should enqueue after storage restarts")
        .unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            StorageMsg::Store { key, .. } if key == recovery_key(TestSettings::RECOVERY_KEY_SUFFIX)
        ));
    }

    #[tokio::test]
    async fn recovery_save_queues_during_a_storage_restart_when_there_is_room() {
        let (commands, _requests) = mpsc::channel(1);
        let overwatch = OverwatchHandle::new(Handle::current(), commands);
        let settings = TestSettings {
            recovery_data: RecoveryData::default(),
        };
        let mut backend = TestBackend::from_settings(&settings, overwatch);

        let (sender, mut receiver) = mpsc::channel(1);
        let (status, watcher) = watch::channel(ServiceStatus::Stopped);
        let storage = StorageApi::new(OutboundRelay::new(sender));
        assert!(
            backend
                .storage
                .set((storage, StatusWatcher::new(watcher)))
                .is_ok()
        );

        let snapshot = TestState {
            value: "snapshot while stopped".into(),
        };
        timeout(Duration::from_secs(1), backend.save_state(snapshot.clone()))
            .await
            .expect("the save should have room to enqueue")
            .unwrap();

        status.send(ServiceStatus::Ready).unwrap();
        let StorageMsg::Store { key, value } = receiver.try_recv().unwrap() else {
            panic!("expected the queued snapshot");
        };

        assert_eq!(key, recovery_key(TestSettings::RECOVERY_KEY_SUFFIX));
        assert_eq!(TestState::from_bytes(&value).unwrap(), snapshot);
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
