use std::fmt::Debug;

use lb_log_targets::utils;
use log::error;
use overwatch::{
    overwatch::OverwatchHandle,
    services::state::{ServiceState, StateOperator},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::overwatch::recovery::{RecoveryResult, errors::RecoveryError};

const LOG_TARGET: &str = utils::RECOVERY;

#[async_trait::async_trait]
pub trait RecoveryBackend<RuntimeServiceId> {
    type State: ServiceState;
    fn from_settings(
        settings: &<Self::State as ServiceState>::Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self;
    fn load_state(
        settings: &<Self::State as ServiceState>::Settings,
    ) -> RecoveryResult<Option<Self::State>>;
    async fn save_state(&mut self, state: Self::State) -> RecoveryResult<()>;
}

#[derive(Debug, Clone)]
pub struct RecoveryOperator<Backend> {
    recovery_backend: Backend,
}

impl<Backend> RecoveryOperator<Backend> {
    const fn new(recovery_backend: Backend) -> Self {
        Self { recovery_backend }
    }
}

#[async_trait::async_trait]
impl<Backend, RuntimeServiceId> StateOperator<RuntimeServiceId> for RecoveryOperator<Backend>
where
    Backend: RecoveryBackend<RuntimeServiceId> + Send,
    Backend::State: Serialize + DeserializeOwned + Send + 'static,
{
    type State = Backend::State;
    type LoadError = RecoveryError;

    fn try_load(
        settings: &<Self::State as ServiceState>::Settings,
    ) -> Result<Option<Self::State>, Self::LoadError> {
        Backend::load_state(settings)
    }

    fn from_settings(
        settings: &<Self::State as ServiceState>::Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        Self::new(Backend::from_settings(settings, overwatch_handle))
    }

    async fn run(&mut self, state: Self::State) {
        let save_result = self.recovery_backend.save_state(state).await;
        if let Err(error) = save_result {
            error!(target: LOG_TARGET, "{error}");
        }
    }
}
