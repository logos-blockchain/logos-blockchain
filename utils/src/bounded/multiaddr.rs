use multiaddr::Multiaddr;
use serde::{Deserialize, Deserializer};

use crate::bounded::{Bounded, BoundedError, BoundedLen, BoundedVec};

impl BoundedLen for Multiaddr {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

/// A `Multiaddr` whose byte length is statically enforced to be in the range
/// `[MIN, MAX]`.
///
/// A thin alias over [`Bounded`]. Length checking, serialization, `Display` and
/// unchecked construction come from the generic wrapper; multiaddr
/// deserialization and the remaining multiaddr-flavoured conversions live
/// here.
pub type BoundedMultiaddr<const MIN: usize, const MAX: usize> = Bounded<Multiaddr, MIN, MAX>;

impl<'de, const MIN: usize, const MAX: usize> Deserialize<'de> for BoundedMultiaddr<MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Multiaddr::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

impl<const MIN: usize, const MAX: usize> BoundedMultiaddr<MIN, MAX> {
    /// Length in bytes (not `char`s), matching `Multiaddr` semantics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_inner().len()
    }

    /// Returns true if the length of this multiaddress is 0, matching
    /// `Multiaddr` semantics.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_inner().is_empty()
    }

    #[must_use]
    pub fn to_vec(&self) -> BoundedVec<u8, MIN, MAX> {
        BoundedVec::new_unchecked(self.as_inner().to_vec())
    }
}

impl<const MIN: usize, const MAX: usize> TryFrom<Multiaddr> for BoundedMultiaddr<MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: Multiaddr) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
