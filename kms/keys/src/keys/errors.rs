use std::any::{type_name, type_name_of_val};

use thiserror::Error;
use tokio::task::JoinError;

use crate::keys::secured_key::SecuredKey;

#[derive(Error, Debug)]
pub enum KeyError {
    #[error(transparent)]
    Encoding(EncodingError),
    #[error("Unsupported multikey: {0}")]
    UnsupportedKey(String),
    #[error("Multisignature support only {0} keys, got {1}")]
    UnsupportedMultisignatureSize(usize, usize),
    #[error(transparent)]
    ZkSignError(#[from] zksign::ZkSignError),
    #[error("Unsupported operator `{operator}` for key type `{key}`")]
    UnsupportedKeyOperator { operator: String, key: String },
    #[error(transparent)]
    FailedOperatorCall(JoinError),
}

impl From<EncodingError> for KeyError {
    fn from(value: EncodingError) -> Self {
        Self::Encoding(value)
    }
}

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum EncodingError {
    #[error("Required encoding: {0}")]
    Requires(String),
}

impl EncodingError {
    /// Creates a new `EncodingError::Requires` error.
    pub fn requires<Key: SecuredKey, Payload>(key: &Key, received_payload: &Payload) -> Self {
        let key_type_name = type_name_of_val(key);
        let payload_type_name = type_name::<Key::Payload>().to_owned();
        let received_payload_type_name = type_name_of_val(received_payload);
        Self::Requires(format!(
            "Key of type `{key_type_name}` requires a payload of type `{payload_type_name}`, but got payload of type `{received_payload_type_name}`",
        ))
    }
}
