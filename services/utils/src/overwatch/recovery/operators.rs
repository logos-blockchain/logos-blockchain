use std::fmt::Debug;

use log::error;
use overwatch::services::state::{ServiceState, StateOperator};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    overwatch::recovery::{RecoveryResult, errors::RecoveryError},
    traits::FromSettings,
};

pub trait RecoveryBackend {
    type State: ServiceState;
    fn load_state(&self) -> RecoveryResult<Self::State>;
    fn save_state(&self, state: &Self::State) -> RecoveryResult<()>;
}

#[derive(Debug, Clone)]
pub struct RecoveryOperator<Backend> {
    recovery_backend: Backend,
}

impl<Backend> RecoveryOperator<Backend>
where
    Backend: RecoveryBackend,
{
    const fn new(recovery_backend: Backend) -> Self {
        Self { recovery_backend }
    }
}

#[async_trait::async_trait]
impl<Backend> StateOperator for RecoveryOperator<Backend>
where
    Backend: RecoveryBackend
        + FromSettings<Settings = <Backend::State as ServiceState>::Settings>
        + Send,
    Backend::State: Serialize + DeserializeOwned + Send,
{
    type State = Backend::State;
    type LoadError = RecoveryError;

    fn try_load(
        settings: &<Self::State as ServiceState>::Settings,
    ) -> Result<Option<Self::State>, Self::LoadError> {
        Backend::from_settings(settings)
            .load_state()
            .map(Option::from)
    }

    fn from_settings(settings: &<Self::State as ServiceState>::Settings) -> Self {
        let recovery_backend = Backend::from_settings(settings);
        Self::new(recovery_backend)
    }

    async fn run(&mut self, state: Self::State) {
        let save_result = self.recovery_backend.save_state(&state);
        if let Err(error) = save_result {
            error!("{error}");
        }
    }
}
