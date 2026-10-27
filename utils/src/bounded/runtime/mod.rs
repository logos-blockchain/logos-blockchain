//! Generic newtypes whose *measured length* is constrained to an inclusive
//! `[MIN, max_len]` range, where `MIN` is static and `max_len` is given at
//! runtime.
//!
//! [`RuntimeBounded`] is [`Bounded`](super::Bounded) for a maximum that is not
//! known when the code is compiled, such as an era's limit. Its length is
//! checked against the maximum it is built or decoded under, and nothing
//! changes it afterwards, so it does not keep the maximum.
//! [`ResizableRuntimeBounded`] keeps it, to hold every later growth to it. The
//! minimum stays static: it says what a collection is (empty or not), not a
//! limit that changes.
//!
//! A maximum is given in an integer type ([`RuntimeMax`]), `u8` for an era's
//! `max_uncles`. The type bounds every maximum a value can be given, and so
//! every length the value holds: the canonical codec writes the length as one,
//! so a value encodes the same under any maximum, and serde, which has no
//! maximum to give, reads a value under the type's largest.

use core::{
    fmt::{self, Display, Formatter},
    marker::PhantomData,
};

use serde::{Serialize, Serializer};

use crate::bounded::{BoundedError, BoundedLen};

pub mod resizable;
pub use resizable::{ResizableRuntimeBounded, ResizableRuntimeBoundedVec};
pub mod vec;
pub use vec::{NonEmptyRuntimeBoundedVec, RuntimeBoundedVec, UpperRuntimeBoundedVec};

/// An integer type runtime maxima are given in.
pub trait RuntimeMax: Copy {
    /// The largest maximum the type gives, which caps the length of every
    /// value bounded in it.
    const MAX: Self;

    /// The value as a `usize`, to compare lengths with.
    fn to_usize(self) -> usize;
}

impl RuntimeMax for u8 {
    const MAX: Self = Self::MAX;

    fn to_usize(self) -> usize {
        usize::from(self)
    }
}

impl RuntimeMax for u16 {
    const MAX: Self = Self::MAX;

    fn to_usize(self) -> usize {
        usize::from(self)
    }
}

impl RuntimeMax for u32 {
    const MAX: Self = Self::MAX;

    fn to_usize(self) -> usize {
        self.try_into()
            .expect("usize should be able to hold u32 on this platform")
    }
}

/// A newtype over `T` whose [measured length](BoundedLen) was checked to lie
/// within the inclusive range `[MIN, max_len]`, `max_len` given as a `Max`.
///
/// The check happens at every *checked* construction site
/// ([`RuntimeBounded::try_new`], decoding under a maximum, deserialization
/// under the largest `Max`). [`RuntimeBounded::new_unchecked`] deliberately
/// bypasses the check and is reserved for callers that have already validated
/// the length (or measure it out-of-band).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeBounded<T, Max, const MIN: usize> {
    inner: T,
    max: PhantomData<Max>,
}

impl<T, Max, const MIN: usize> RuntimeBounded<T, Max, MIN> {
    pub const MIN: usize = MIN;

    /// Wrap `inner` without checking the bound.
    ///
    /// Reserved for callers that have already validated the length. Prefer
    /// [`Self::try_new`] at trust boundaries.
    #[must_use]
    pub const fn new_unchecked(inner: T) -> Self {
        Self {
            inner,
            max: PhantomData,
        }
    }

    /// Borrow the wrapped value.
    #[must_use]
    pub const fn as_inner(&self) -> &T {
        &self.inner
    }

    /// Consume the wrapper and return the inner value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Validate a length against the range `[MIN, max_len]`.
    ///
    /// This is the single source of truth for the bound. Types that measure
    /// themselves out-of-band (because they cannot implement [`BoundedLen`])
    /// call this directly, then wrap with [`Self::new_unchecked`]. A `max_len`
    /// below `MIN` admits no length.
    pub(crate) const fn check_len_against_bounds(
        len: usize,
        max_len: usize,
    ) -> Result<(), BoundedError> {
        if len < MIN {
            return Err(BoundedError::too_few(len, MIN));
        }
        if len > max_len {
            return Err(BoundedError::TooManyItems {
                count: len,
                max: max_len,
            });
        }
        Ok(())
    }
}

impl<T, Max, const MIN: usize> RuntimeBounded<T, Max, MIN>
where
    T: BoundedLen,
    Max: RuntimeMax,
{
    /// Checked constructor for any [measurable](BoundedLen) `T`.
    pub fn try_new(inner: T, max_len: Max) -> Result<Self, BoundedError> {
        Self::check_len_against_bounds(inner.bounded_len(), max_len.to_usize())?;
        Ok(Self::new_unchecked(inner))
    }
}

impl<T, Max, const MIN: usize> AsRef<T> for RuntimeBounded<T, Max, MIN> {
    fn as_ref(&self) -> &T {
        &self.inner
    }
}

impl<T, Max, const MIN: usize> Display for RuntimeBounded<T, Max, MIN>
where
    T: Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

// Transparent serialization: a `RuntimeBounded<T, ..>` is wire-identical to its
// inner `T`.
impl<T, Max, const MIN: usize> Serialize for RuntimeBounded<T, Max, MIN>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.inner.serialize(serializer)
    }
}
