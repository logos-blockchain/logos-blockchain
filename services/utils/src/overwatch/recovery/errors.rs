#[derive(thiserror::Error, Debug)]
pub enum RecoveryError {
    #[error("Recovery backend error: {0}")]
    Backend(String),
    #[error("Recovery backend is not running: {0}")]
    NotRunning(String),
}
