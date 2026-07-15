use std::{marker::PhantomData, path::PathBuf};

use overwatch::{overwatch::OverwatchHandle, services::state::ServiceState};

use crate::{
    overwatch::recovery::{
        RecoveryResult,
        errors::RecoveryError,
        operators::RecoveryBackend,
        serializer::{JsonRecoverySerializer, RecoverySerializer},
    },
    traits::FromSettings,
};

pub trait FileBackendSettings {
    fn recovery_file(&self) -> &PathBuf;
}

#[derive(Debug, Clone)]
pub struct FileBackend<State, RecoverySettings, Serializer> {
    recovery_file: PathBuf,
    state: PhantomData<State>,
    recovery_settings: PhantomData<RecoverySettings>,
    serializer: PhantomData<Serializer>,
}

impl<State, RecoverySettings, Serializer> FileBackend<State, RecoverySettings, Serializer> {
    #[must_use]
    pub const fn recovery_file(&self) -> &PathBuf {
        &self.recovery_file
    }
}

impl<State, RecoverySettings, Serializer> FromSettings
    for FileBackend<State, RecoverySettings, Serializer>
where
    RecoverySettings: FileBackendSettings,
{
    type Settings = RecoverySettings;

    fn from_settings(settings: &Self::Settings) -> Self {
        Self {
            recovery_file: settings.recovery_file().clone(),
            state: PhantomData,
            serializer: PhantomData,
            recovery_settings: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<State, RecoverySettings, Serializer, RuntimeServiceId> RecoveryBackend<RuntimeServiceId>
    for FileBackend<State, RecoverySettings, Serializer>
where
    State: ServiceState<Settings = RecoverySettings> + Send,
    RecoverySettings: FileBackendSettings + Send,
    Serializer: RecoverySerializer<State = State> + Send,
{
    type State = State;

    fn from_settings(
        settings: &RecoverySettings,
        _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        <Self as FromSettings>::from_settings(settings)
    }

    fn load_state(settings: &RecoverySettings) -> RecoveryResult<Option<Self::State>> {
        let Ok(serialized_state) = std::fs::read_to_string(settings.recovery_file()) else {
            return Ok(None);
        };
        Ok(Serializer::deserialize(&serialized_state).ok())
    }

    async fn save_state(&mut self, state: Self::State) -> RecoveryResult<()> {
        let serialized_state = Serializer::serialize(&state)?;
        if let Some(parent) = self.recovery_file.parent() {
            std::fs::create_dir_all(parent).map_err(RecoveryError::from)?;
        }
        std::fs::write(&self.recovery_file, serialized_state).map_err(RecoveryError::from)
    }
}

pub type JsonFileBackend<State, Settings> =
    FileBackend<State, Settings, JsonRecoverySerializer<State>>;
