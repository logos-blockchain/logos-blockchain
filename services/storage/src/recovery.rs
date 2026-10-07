use std::{cmp::Ordering, fmt::Display, marker::PhantomData, sync::Arc};

use bytes::Bytes;
use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::era::{ForkDigest, ForkDigests};
use lb_log_targets::utils;
pub use lb_services_utils::overwatch::recovery::StorageRecoverySettings;
use lb_services_utils::overwatch::recovery::{
    RecoveryBackend, RecoveryData, RecoveryError, RecoveryResult, VersionedState,
    versioned::StateVersion,
};
use overwatch::{
    DynError,
    overwatch::OverwatchHandle,
    services::{AsServiceId, state::ServiceState},
};
use serde::{Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use tokio::sync::OnceCell;
use tracing::warn;

use crate::{
    StorageService,
    api::StorageApi,
    backend::StorageBackend as _,
    rocksdb::{RocksBackend, RocksBackendSettings},
};

const LOG_TARGET: &str = utils::RECOVERY;

const RECOVERY_PREFIX: &[u8] = b"recovery/";

#[must_use]
pub fn recovery_key(suffix: &[u8]) -> Bytes {
    let mut key = Vec::with_capacity(RECOVERY_PREFIX.len() + suffix.len());
    key.extend_from_slice(RECOVERY_PREFIX);
    key.extend_from_slice(suffix);
    key.into()
}

/// The recovery records in storage, for the chain whose eras are represented by
/// the fork digests `forks`.
pub fn load_recovery_data(
    settings: RocksBackendSettings,
    forks: Arc<ForkDigests>,
) -> Result<RecoveryData, DynError> {
    let backend = RocksBackend::new(settings)?;
    recovery_data_from_backend(&backend, forks)
}

fn recovery_data_from_backend(
    backend: &RocksBackend,
    forks: Arc<ForkDigests>,
) -> Result<RecoveryData, DynError> {
    backend
        .load_prefix_entries(RECOVERY_PREFIX)
        .map(|entries| RecoveryData::new(entries, forks))
        .map_err(Into::into)
}

/// Takes the record under `key_suffix` out of `data` and reads the state it
/// carries, brought to the version this release writes.
///
/// A record written under a different fork digest is discarded with a warning.
///
/// # Errors
///
/// If the record is of a newer version than the state's, or if its state
/// cannot be read or migrated.
pub fn take_state<State>(data: &RecoveryData, key_suffix: &[u8]) -> RecoveryResult<Option<State>>
where
    State: VersionedState + DeserializeOwned,
{
    let key = recovery_key(key_suffix);
    let Some(record) = data.take(&key)? else {
        return Ok(None);
    };
    let Some((state_version, fork_digest, state)) = read_record(&record) else {
        warn!(
            target: LOG_TARGET,
            "Discarding the recovery record {}: unexpected EOF.",
            String::from_utf8_lossy(&key)
        );
        return Ok(None);
    };
    if !data
        .forks()
        .iter()
        .any(|era| era.entry.parameters == fork_digest)
    {
        warn!(
            target: LOG_TARGET,
            "Discarding the recovery record {}, written on another chain, under fork {fork_digest}",
            String::from_utf8_lossy(&key),
        );
        return Ok(None);
    }
    match state_version.cmp(&State::VERSION) {
        Ordering::Equal => State::from_bytes(state)
            .map(Some)
            .map_err(|error| RecoveryError::Backend(error.to_string())),
        Ordering::Less => State::migrate(state_version, state)
            .map(Some)
            .map_err(|error| RecoveryError::Migration {
                from: state_version,
                error,
            }),
        Ordering::Greater => Err(RecoveryError::NewerVersion {
            found: state_version,
            current: State::VERSION,
        }),
    }
}

type StateVersionEncodeFn = fn(StateVersion) -> [u8; 2];
type StateVersionDecodeFn = fn([u8; 2]) -> StateVersion;
fn state_version_codecs() -> (StateVersionEncodeFn, StateVersionDecodeFn) {
    let encode_fn = |version: StateVersion| version.get().to_le_bytes();
    let decode_fn = |bytes: [u8; 2]| StateVersion::new(u16::from_le_bytes(bytes));
    (encode_fn, decode_fn)
}

type ForkDigestEncodeFn = fn(ForkDigest) -> [u8; 32];
type ForkDigestDecodeFn = fn([u8; 32]) -> ForkDigest;
fn fork_digest_codecs() -> (ForkDigestEncodeFn, ForkDigestDecodeFn) {
    let encode_fn = |fork_digest: ForkDigest| fork_digest.into();
    let decode_fn = |bytes: [u8; 32]| bytes.into();
    (encode_fn, decode_fn)
}

/// The recovery record of `state`: the version of the state's
/// layout and the fork digest of the era in force when it is written, then the
/// state.
fn write_record(state_version: StateVersion, fork_digest: ForkDigest, state: &[u8]) -> Bytes {
    [
        state_version_codecs().0(state_version).as_slice(),
        fork_digest_codecs().0(fork_digest).as_slice(),
        state,
    ]
    .concat()
    .into()
}

/// The state version, the fork digest and the state of `record`, as
/// [`write_record`] lays them out. `None` when the record is too short to
/// carry the necessary metadata.
fn read_record(record: &[u8]) -> Option<(StateVersion, ForkDigest, &[u8])> {
    let (state_version, record) = record.split_first_chunk()?;
    let (fork_digest, state) = record.split_first_chunk()?;
    Some((
        state_version_codecs().1(*state_version),
        fork_digest_codecs().1(*fork_digest),
        state,
    ))
}

fn fork_in_force(forks: &ForkDigests, time: OffsetDateTime) -> ForkDigest {
    forks
        .at_time(time)
        .unwrap_or_else(|| forks.genesis())
        .entry
        .parameters
}

pub struct StorageRecoveryBackend<State, Settings, RuntimeServiceId> {
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    storage: OnceCell<StorageApi>,
    /// The fork digest of every era of the chain, the one in force stamping
    /// each record written.
    forks: Arc<ForkDigests>,
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
            forks: Arc::clone(&self.forks),
            state: PhantomData,
            settings: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<State, Settings, RuntimeServiceId> RecoveryBackend<RuntimeServiceId>
    for StorageRecoveryBackend<State, Settings, RuntimeServiceId>
where
    State: ServiceState<Settings = Settings> + VersionedState + Serialize + DeserializeOwned + Send,
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
        settings: &Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        Self {
            overwatch_handle,
            storage: OnceCell::new(),
            forks: Arc::clone(settings.recovery_data().forks()),
            state: PhantomData,
            settings: PhantomData,
        }
    }

    fn load_state(settings: &Settings) -> RecoveryResult<Option<Self::State>> {
        take_state(settings.recovery_data(), Settings::RECOVERY_KEY_SUFFIX)
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
        let state = state
            .to_bytes()
            .map_err(|error| RecoveryError::Backend(error.to_string()))?;
        let record = write_record(
            State::VERSION,
            fork_in_force(&self.forks, OffsetDateTime::now_utc()),
            &state,
        );

        storage
            .store_raw(recovery_key(Settings::RECOVERY_KEY_SUFFIX), record)
            .await
            .map_err(|error| RecoveryError::Backend(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, num::NonZero, time::Duration};

    use lb_cryptarchia_engine::era::{EraEntriesAfterGenesis, EraEntry, EraSchedule};
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

    /// A state at version 2, whose versions before were a bare string.
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

    impl VersionedState for TestState {
        const VERSION: StateVersion = StateVersion::new(2);

        fn migrate(from: StateVersion, bytes: &[u8]) -> Result<Self, DynError> {
            let value = String::from_bytes(bytes)?;
            Ok(Self {
                value: format!("{value}, migrated from version {from}"),
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

    const ERA_0: [u8; 32] = [1; 32];
    const ERA_1: [u8; 32] = [2; 32];

    /// A chain of two eras of 10 one-second slots an epoch, from the Unix
    /// epoch: era 1 starts at slot 10, 10 seconds in.
    fn forks() -> Arc<ForkDigests> {
        let era = |parameters| EraEntry {
            slot_duration: Duration::from_secs(1),
            epoch_length_in_slots: NonZero::new(10).unwrap(),
            transition_slots: 0,
            parameters,
        };
        Arc::new(
            EraSchedule::new(
                OffsetDateTime::UNIX_EPOCH,
                era(ERA_0.into()),
                EraEntriesAfterGenesis::from((NonZero::new(1).unwrap(), era(ERA_1.into()))),
            )
            .unwrap(),
        )
    }

    fn settings_with_record(record: impl Into<Bytes>) -> TestSettings {
        TestSettings {
            recovery_data: RecoveryData::new(
                HashMap::from([(
                    recovery_key(TestSettings::RECOVERY_KEY_SUFFIX).to_vec(),
                    record.into(),
                )]),
                forks(),
            ),
        }
    }

    fn load(settings: &TestSettings) -> RecoveryResult<Option<TestState>> {
        <TestBackend as RecoveryBackend<TestRuntimeServiceId>>::load_state(settings)
    }

    fn rocks_backend(directory: &tempfile::TempDir) -> RocksBackend {
        RocksBackend::new(RocksBackendSettings {
            db_path: directory.path().into(),
            read_only: false,
            column_family: None,
        })
        .unwrap()
    }

    #[test]
    fn loads_and_removes_state_from_configured_key() {
        let expected = TestState {
            value: "restored".into(),
        };
        let directory = tempfile::tempdir().unwrap();
        let reader = rocks_backend(&directory);
        let record = write_record(
            StateVersion::new(2),
            ERA_0.into(),
            &expected.to_bytes().unwrap(),
        );
        reader
            .txn(move |database| {
                database.put(recovery_key(TestSettings::RECOVERY_KEY_SUFFIX), record)?;
                Ok(None)
            })
            .execute()
            .unwrap();
        let recovery_data = recovery_data_from_backend(&reader, forks()).unwrap();
        let settings = TestSettings { recovery_data };

        assert_eq!(load(&settings).unwrap(), Some(expected));
        assert!(load(&settings).unwrap().is_none());
    }

    #[test]
    fn missing_recovery_key_returns_none() {
        let directory = tempfile::tempdir().unwrap();
        let backend = rocks_backend(&directory);
        let recovery_data = recovery_data_from_backend(&backend, forks()).unwrap();
        let settings = TestSettings { recovery_data };

        assert!(load(&settings).unwrap().is_none());
        assert!(load(&settings).unwrap().is_none());
    }

    #[test]
    fn records_are_stamped_with_the_fork_of_the_era_in_force() {
        let forks = forks();
        let at = |seconds| OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seconds);

        assert_eq!(fork_in_force(&forks, at(-5)), ForkDigest::from(ERA_0));
        assert_eq!(fork_in_force(&forks, at(9)), ForkDigest::from(ERA_0));
        assert_eq!(fork_in_force(&forks, at(10)), ForkDigest::from(ERA_1));
    }

    #[test]
    fn records_of_every_era_of_the_chain_are_read() {
        for fork in [ERA_0, ERA_1] {
            let state = TestState {
                value: "restored".into(),
            };
            let settings = settings_with_record(write_record(
                StateVersion::new(2),
                fork.into(),
                &state.to_bytes().unwrap(),
            ));

            assert_eq!(load(&settings).unwrap(), Some(state));
        }
    }

    #[test]
    fn records_of_another_chain_are_discarded() {
        let state = TestState {
            value: "elsewhere".into(),
        };
        let settings = settings_with_record(write_record(
            StateVersion::new(2),
            [3; 32].into(),
            &state.to_bytes().unwrap(),
        ));

        assert!(load(&settings).unwrap().is_none());
    }

    #[test]
    fn older_versions_are_migrated() {
        let settings = settings_with_record(write_record(
            StateVersion::new(1),
            ERA_1.into(),
            &"old".to_bytes().unwrap(),
        ));

        assert_eq!(
            load(&settings).unwrap(),
            Some(TestState {
                value: "old, migrated from version 1".into()
            })
        );
    }

    #[test]
    fn newer_versions_are_refused() {
        let settings = settings_with_record(write_record(
            StateVersion::new(3),
            ERA_0.into(),
            b"from a later release",
        ));

        assert!(matches!(
            load(&settings),
            Err(RecoveryError::NewerVersion { found, current })
                if found == StateVersion::new(3) && current == StateVersion::new(2)
        ));
    }

    #[test]
    fn states_that_do_not_read_at_their_version_are_errors() {
        let settings = settings_with_record(write_record(
            StateVersion::new(2),
            ERA_0.into(),
            b"invalid recovery state",
        ));

        assert!(matches!(load(&settings), Err(RecoveryError::Backend(_))));
        assert!(load(&settings).unwrap().is_none());
    }

    #[test]
    fn records_too_short_to_carry_a_stamp_are_discarded() {
        let settings = settings_with_record(&b"too short"[..]);

        assert!(load(&settings).unwrap().is_none());
    }
}
