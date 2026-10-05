use core::fmt::{self, Display};

use overwatch::DynError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateVersion(u16);

impl StateVersion {
    #[must_use]
    pub const fn new(version: u16) -> Self {
        Self(version)
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Display for StateVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A service state kept in recovery records, which carry the version of its
/// layout.
///
/// A release that changes the layout of a state, or what it means, bumps its
/// version and teaches [`Self::migrate`] to read the versions before it, like
/// a pallet's storage version. A record of a newer version than the release
/// knows is refused: the node runs an older release than the one that wrote
/// it.
pub trait VersionedState: Sized {
    /// The version of the layout this release writes.
    const VERSION: StateVersion;

    /// Reads a state written at the older version `from` and brings it to
    /// [`Self::VERSION`]. Version 0 is a record written before records
    /// carried a version.
    ///
    /// # Errors
    ///
    /// If `bytes` are not a state of version `from`.
    fn migrate(from: StateVersion, bytes: &[u8]) -> Result<Self, DynError>;
}
