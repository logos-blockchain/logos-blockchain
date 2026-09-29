//! A generic newtype whose *measured length* is statically constrained to an
//! inclusive `[MIN, MAX]` range.
//!
//! [`Bounded`] captures the machinery shared by every length-bounded type in
//! the codebase — bound checking, unchecked/checked construction, and
//! transparent serialization — so that concrete bounded
//! types (`BoundedVec`, chain IDs, locators, …) reduce to a type alias plus
//! whatever operations are natural for the wrapped type.
//!
//! What "length" means is delegated to [`BoundedLen`]: element count for
//! collections, byte length for strings, and so on. Types that cannot
//! implement [`BoundedLen`] (e.g. foreign types this crate does not depend on)
//! can still reuse the bound-checking logic via
//! [`Bounded::check_len_against_bounds`] and
//! [`Bounded::new_unchecked`].
//!
//! [`BoundedOrderedSet`] and [`BoundedOrderedMap`] add a second invariant on
//! top of the bound: every checked construction path rejects a repeated
//! element or key instead of silently merging it, so the number of items read
//! is always the number of items held.

use core::fmt::{self, Display, Formatter};

use serde::{Serialize, Serializer};
use thiserror::Error;

pub mod multiaddr;
pub mod ordered_map;
pub use ordered_map::{BoundedOrderedMap, NonEmptyBoundedOrderedMap, UpperBoundedOrderedMap};
pub mod ordered_set;
pub use ordered_set::{BoundedOrderedSet, NonEmptyBoundedOrderedSet, UpperBoundedOrderedSet};
pub mod string;
pub use string::BoundedString;
pub mod vec;
pub use vec::{
    BoundedVec, LowerBoundedVec, MaxBoundedVec, NonEmptyBoundedVec, UpperBoundedVec,
    deserialize_bounded_sequence,
};

mod collection;

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
    #[error("Requested capacity {capacity} is out of bounds [{min}, {max}]")]
    CapacityOutOfBounds {
        min: usize,
        max: usize,
        capacity: usize,
    },
    /// Raised by [`BoundedOrderedSet`] and [`BoundedOrderedMap`]: the item at
    /// `index` (0-based, in input order) repeats an earlier element or key.
    #[error("Item at index {index} is a duplicate of an earlier item")]
    DuplicateItem { index: usize },
}

impl BoundedError {
    /// The error for `count` items against a minimum of `min`: an empty input
    /// has its own variant.
    const fn too_few(count: usize, min: usize) -> Self {
        if count == 0 {
            Self::EmptyInput
        } else {
            Self::TooFewItems { count, min }
        }
    }
}

/// The most memory, in bytes, a bounded collection reserves up front for a
/// length it has been told about but has not yet seen.
const MAX_PREALLOCATION_BYTES: usize = 1024 * 1024;

/// How many items a `MAX`-bounded collection should reserve room for, given a
/// declared length.
///
/// A declared length is only a claim. In a binary format it is read off the
/// wire before a single item has been seen, so trusting it up to `MAX` would
/// let a few bytes of input reserve `MAX` items' worth of memory, or, for a
/// large `MAX`, overflow the allocator and panic. The claim is honoured up to
/// [`MAX_PREALLOCATION_BYTES`]; past that the collection grows as items
/// actually arrive. Zero-sized items need no room, so they get none.
pub(crate) fn allocation_size_for_hint<Item, const MAX: usize>(hint: Option<usize>) -> usize {
    let item_size = size_of::<Item>();
    if item_size == 0 {
        return 0;
    }
    hint.unwrap_or(0)
        .min(MAX)
        .min(MAX_PREALLOCATION_BYTES / item_size)
}

/// The measured length of a value, in whatever unit is natural for its type:
/// element count for collections, byte length for text.
///
/// Implementing this for a type unlocks the ergonomic checked constructors on
/// [`Bounded`] ([`Bounded::try_new`] and the concrete `TryFrom` impls).
/// Foreign types that cannot get an impl here can still be bounded manually via
/// [`Bounded::check_len_against_bounds`] + [`Bounded::new_unchecked`].
pub trait BoundedLen {
    fn bounded_len(&self) -> usize;
}

/// A newtype over `T` whose [measured length](BoundedLen) is statically
/// enforced to lie within the inclusive range `[MIN, MAX]`.
///
/// The invariant holds at every *checked* construction site
/// ([`Bounded::try_new`], the concrete `TryFrom` impls, deserialization).
/// [`Bounded::new_unchecked`] deliberately bypasses the check and is reserved
/// for callers that have already validated the length (or measure it
/// out-of-band).
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Bounded<T, const MIN: usize, const MAX: usize>(T);

impl<T, const MIN: usize, const MAX: usize> Bounded<T, MIN, MAX> {
    pub const MIN: usize = MIN;
    pub const MAX: usize = MAX;

    /// Wrap `inner` without checking the bound.
    ///
    /// Reserved for callers that have already validated the length. Prefer
    /// [`Self::try_new`] (or a concrete `TryFrom`) at trust boundaries.
    #[must_use]
    pub const fn new_unchecked(inner: T) -> Self {
        const { assert!(MIN <= MAX, "Bounded MIN must not exceed MAX") }
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
        const { assert!(MIN <= MAX, "Bounded MIN must not exceed MAX") }
        if len < MIN {
            return Err(BoundedError::too_few(len, MIN));
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

#[cfg(test)]
mod tests {
    use super::{MAX_PREALLOCATION_BYTES, allocation_size_for_hint};

    #[test]
    fn preallocation_honours_a_small_declared_length() {
        assert_eq!(allocation_size_for_hint::<u64, 16>(Some(3)), 3);
    }

    #[test]
    fn preallocation_is_zero_without_a_declared_length() {
        assert_eq!(allocation_size_for_hint::<u64, 16>(None), 0);
    }

    #[test]
    fn preallocation_never_exceeds_max() {
        assert_eq!(allocation_size_for_hint::<u64, 16>(Some(1_000)), 16);
    }

    #[test]
    fn preallocation_never_exceeds_the_byte_budget() {
        let capacity = allocation_size_for_hint::<[u8; 32], { usize::MAX }>(Some(usize::MAX));

        assert_eq!(capacity, MAX_PREALLOCATION_BYTES / 32);
    }

    #[test]
    fn preallocation_reserves_nothing_for_zero_sized_items() {
        assert_eq!(
            allocation_size_for_hint::<(), { usize::MAX }>(Some(usize::MAX)),
            0
        );
    }
}
