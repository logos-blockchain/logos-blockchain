use multiaddr::Multiaddr;

use crate::bounded::{Bounded, BoundedError, BoundedLen};

impl BoundedLen for Multiaddr {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

/// A `Multiaddr` whose byte length is statically enforced to be in the range
/// `[MIN, MAX]`.
///
/// A thin alias over [`Bounded`]. Length checking, (de)serialization, `Display`
/// and unchecked construction all come from the generic wrapper; only the
/// multiaddr-flavoured conversions live here.
pub type BoundedMultiaddr<const MIN: usize, const MAX: usize> = Bounded<Multiaddr, MIN, MAX>;

impl<const MIN: usize, const MAX: usize> BoundedMultiaddr<MIN, MAX> {
    /// Length in bytes (not `char`s), matching `Multiaddr` semantics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_inner().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_inner().is_empty()
    }
}

impl<const MIN: usize, const MAX: usize> TryFrom<Multiaddr> for BoundedMultiaddr<MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: Multiaddr) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
