use overwatch::DynError;

use crate::overwatch::recovery::versioned::StateVersion;

#[derive(thiserror::Error, Debug)]
pub enum RecoveryError {
    #[error("Recovery backend error: {0}")]
    Backend(String),
    #[error(
        "Recovered state is at version {found}, newer than version {current}, the one this release writes"
    )]
    NewerVersion {
        found: StateVersion,
        current: StateVersion,
    },
    #[error("Recovered state at version {from} could not be migrated: {error}")]
    Migration { from: StateVersion, error: DynError },
}
