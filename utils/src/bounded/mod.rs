//! A generic newtype whose *measured length* is statically constrained to an
//! inclusive `[MIN, MAX]` range.
//!
//! [`Bounded`] captures the machinery shared by every length-bounded type in
//! the codebase — bound checking, unchecked/checked construction, transparent
//! serialization and validating deserialization — so that concrete bounded
//! types (`BoundedVec`, chain IDs, locators, …) reduce to a type alias plus
//! whatever operations are natural for the wrapped type.
//!
//! What "length" means is delegated to [`BoundedLen`]: element count for
//! collections, byte length for strings, and so on. Types that cannot
//! implement [`BoundedLen`] (e.g. foreign types this crate does not depend on)
//! can still reuse the bound-checking logic via [`Bounded::check_len`] and
//! [`Bounded::new_unchecked`].

pub mod multiaddr;
pub mod string;
pub mod vec;

use core::fmt::{self, Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
pub use string::BoundedString;
use thiserror::Error;
pub use vec::{BoundedVec, LowerBoundedVec, MaxBoundedVec, NonEmptyBoundedVec, UpperBoundedVec};

#[derive(Debug, Error, Eq, PartialEq, Clone)]
pub enum BoundedError {
    #[error("Input cannot be empty.")]
    EmptyInput,
    #[error("Item count {count} is below minimum of {min}")]
    TooFewItems { count: usize, min: usize },
    #[error("Item count {count} exceeds static maximum of {max}")]
    TooManyItems { count: usize, max: usize },
    #[error("Index {index} is out of bounds for length {len}")]
    IndexOutOfBounds { index: usize, len: usize },
}

/// The measured length of a value, in whatever unit is natural for its type:
/// element count for collections, byte length for text.
///
/// Implementing this for a type unlocks the ergonomic checked constructors on
/// [`Bounded`] ([`Bounded::new`], `TryFrom`, `Deserialize`, [`Bounded::len`]).
/// Foreign types that cannot get an impl here can still be bounded manually via
/// [`Bounded::check_len`] + [`Bounded::new_unchecked`].
pub trait BoundedLen {
    fn bounded_len(&self) -> usize;
}

/// A newtype over `T` whose [measured length](BoundedLen) is statically
/// enforced to lie within the inclusive range `[MIN, MAX]`.
///
/// The invariant holds at every *checked* construction site ([`Bounded::new`],
/// the concrete `TryFrom` impls, deserialization). [`Bounded::new_unchecked`]
/// deliberately bypasses the check and is reserved for callers that have
/// already validated the length (or measure it out-of-band).
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Bounded<T, const MIN: usize, const MAX: usize>(T);

impl<T, const MIN: usize, const MAX: usize> Bounded<T, MIN, MAX> {
    pub const MIN: usize = MIN;
    pub const MAX: usize = MAX;

    /// Wrap `inner` without checking the bound.
    ///
    /// Reserved for callers that have already validated the length. Prefer
    /// [`Self::new`] (or a concrete `TryFrom`) at trust boundaries.
    #[must_use]
    pub const fn new_unchecked(inner: T) -> Self {
        Self(inner)
    }

    /// Borrow the wrapped value.
    #[must_use]
    pub const fn as_inner(&self) -> &T {
        &self.0
    }

    /// Consume the wrapper and return the inner value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }

    /// Validate a length against the static `[MIN, MAX]` range.
    ///
    /// This is the single source of truth for the bound. Types that measure
    /// themselves out-of-band (because they cannot implement [`BoundedLen`])
    /// call this directly, then wrap with [`Self::new_unchecked`].
    pub const fn check_len_against_bounds(len: usize) -> Result<(), BoundedError> {
        if len < MIN {
            if len == 0 {
                return Err(BoundedError::EmptyInput);
            }
            return Err(BoundedError::TooFewItems {
                count: len,
                min: MIN,
            });
        }
        if len > MAX {
            return Err(BoundedError::TooManyItems {
                count: len,
                max: MAX,
            });
        }
        Ok(())
    }

    /// Checked constructor for any [measurable](BoundedLen) `T`.
    ///
    /// `TryFrom` cannot be blanket-implemented over a bare `T` (it collides
    /// with the standard-library `TryFrom<U> for T where U: Into<T>` impl),
    /// so this inherent method is the shared entry point that the concrete
    /// `TryFrom` impls delegate to.
    pub fn try_new(inner: T) -> Result<Self, BoundedError>
    where
        T: BoundedLen,
    {
        Self::check_len_against_bounds(inner.bounded_len())?;
        Ok(Self(inner))
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<T> for Bounded<T, MIN, MAX> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> Display for Bounded<T, MIN, MAX>
where
    T: Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// Transparent serialization: a `Bounded<T, ..>` is wire-identical to its inner
// `T`, and does not require `T: Clone` (unlike a serde `into = "..."` shim).
impl<T, const MIN: usize, const MAX: usize> Serialize for Bounded<T, MIN, MAX>
where
    T: Serialize,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

// Deserialize the inner `T`, then re-establish the bound before wrapping.
impl<'de, T, const MIN: usize, const MAX: usize> Deserialize<'de> for Bounded<T, MIN, MAX>
where
    T: BoundedLen + Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let inner = T::deserialize(deserializer)?;
        Self::try_new(inner).map_err(serde::de::Error::custom)
    }
}
