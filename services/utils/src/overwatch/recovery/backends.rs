use std::{marker::PhantomData, path::PathBuf};

use overwatch::services::state::ServiceState;

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

impl<State, RecoverySettings, Serializer> RecoveryBackend
    for FileBackend<State, RecoverySettings, Serializer>
where
    State: ServiceState,
    Serializer: RecoverySerializer<State = State>,
{
    type State = State;

    fn load_state(&self) -> RecoveryResult<Self::State> {
        let serialized_state =
            std::fs::read_to_string(&self.recovery_file).map_err(RecoveryError::from)?;
        Serializer::deserialize(&serialized_state)
    }

    fn save_state(&self, state: &Self::State) -> RecoveryResult<()> {
        let serialized_state = Serializer::serialize(state)?;
        std::fs::write(&self.recovery_file, serialized_state).map_err(RecoveryError::from)
    }
}

pub type JsonFileBackend<State, Settings> =
    FileBackend<State, Settings, JsonRecoverySerializer<State>>;
