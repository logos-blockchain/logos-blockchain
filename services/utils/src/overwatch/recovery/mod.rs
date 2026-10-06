pub mod data;
pub mod errors;
pub mod operators;
pub mod versioned;

pub use data::{RecoveryData, StorageRecoverySettings};
pub use errors::RecoveryError;
pub use operators::{RecoveryBackend, RecoveryOperator};
pub use versioned::{StateVersion, VersionedState};

pub type RecoveryResult<T> = Result<T, RecoveryError>;
