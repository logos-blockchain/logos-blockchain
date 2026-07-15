use std::io;

#[derive(thiserror::Error, Debug)]
pub enum RecoveryError {
    #[error(transparent)]
    IoError(#[from] io::Error),
    #[error(transparent)]
    SerdeError(#[from] serde_json::Error),
    #[error("Recovery backend error: {0}")]
    Backend(String),
}
