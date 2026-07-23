pub mod data;
pub mod errors;
pub mod operators;

pub use data::{RecoveryData, StorageRecoverySettings};
pub use errors::RecoveryError;
pub use operators::{RecoveryBackend, RecoveryOperator};

pub type RecoveryResult<T> = Result<T, RecoveryError>;
