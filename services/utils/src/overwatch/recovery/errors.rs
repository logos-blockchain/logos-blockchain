use std::io;

#[derive(thiserror::Error, Debug)]
pub enum RecoveryError {
    #[error(transparent)]
    IoError(#[from] io::Error),
    #[error(transparent)]
    SerdeError(#[from] serde_json::Error),
}
