use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};

use crate::overwatch::{RecoveryError, recovery::RecoveryResult};

pub trait RecoverySerializer {
    type State;

    fn serialize(state: &Self::State) -> RecoveryResult<String>;
    fn deserialize(serialized_state: &str) -> RecoveryResult<Self::State>;
}

#[derive(Debug, Clone)]
pub struct JsonRecoverySerializer<State> {
    state: PhantomData<State>,
}

impl<State: Serialize + DeserializeOwned> RecoverySerializer for JsonRecoverySerializer<State> {
    type State = State;

    fn serialize(state: &Self::State) -> RecoveryResult<String> {
        serde_json::to_string(state).map_err(RecoveryError::from)
    }

    fn deserialize(serialized_state: &str) -> RecoveryResult<Self::State> {
        serde_json::from_str(serialized_state).map_err(RecoveryError::from)
    }
}
