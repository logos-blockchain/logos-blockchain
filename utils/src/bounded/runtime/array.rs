use core::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};

use crate::bounded::{
    BoundedError,
    runtime::{NonEmptyRuntimeBoundedVec, RuntimeMax},
};

/// An array whose length is given at runtime: `[T; N]` for an `N` such as an
/// era's, at least one, given as a `Len`.
///
/// It holds exactly the length it was built or decoded under. Its items can be
/// replaced through the slice it dereferences to, which cannot add or remove
/// any, so that length stays.
///
/// As for an array, the length is not part of the canonical encoding: only the
/// items are written, and the decoder takes the length from its context. Serde,
/// which has no context to take it from, keeps it with the items, and reads an
/// array of any length a `Len` gives.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    transparent,
    bound(
        serialize = "T: Serialize",
        deserialize = "T: Deserialize<'de>, Len: RuntimeMax"
    )
)]
pub struct RuntimeArray<T, Len>(NonEmptyRuntimeBoundedVec<T, Len>);

impl<T, Len> RuntimeArray<T, Len> {
    /// Wraps `items` without checking how many they are.
    ///
    /// Reserved for callers that have already counted them. Prefer
    /// [`Self::try_new`] at trust boundaries.
    #[must_use]
    pub const fn new_unchecked(items: Vec<T>) -> Self {
        Self(NonEmptyRuntimeBoundedVec::new_unchecked(items))
    }
}

impl<T, Len> RuntimeArray<T, Len>
where
    Len: RuntimeMax,
{
    /// Checked constructor: `items` must hold exactly `len` items, at least
    /// one.
    pub fn try_new(items: Vec<T>, len: Len) -> Result<Self, BoundedError> {
        if items.len() < len.to_usize() {
            return Err(BoundedError::too_few(items.len(), len.to_usize()));
        }
        NonEmptyRuntimeBoundedVec::try_new(items, len).map(Self)
    }
}

impl<T, Len> Deref for RuntimeArray<T, Len> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.0.as_inner()
    }
}

impl<T, Len> DerefMut for RuntimeArray<T, Len> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.inner
    }
}

#[cfg(test)]
mod tests {
    use crate::bounded::{BoundedError, RuntimeArray};

    type Array = RuntimeArray<u8, u8>;

    #[test]
    fn it_holds_exactly_its_length() {
        let array = Array::try_new(vec![1, 2], 2).unwrap();

        assert_eq!(*array, [1, 2]);
        assert_eq!(
            Array::try_new(vec![1], 2).unwrap_err(),
            BoundedError::TooFewItems { count: 1, min: 2 }
        );
        assert_eq!(
            Array::try_new(vec![1, 2, 3], 2).unwrap_err(),
            BoundedError::TooManyItems { count: 3, max: 2 }
        );
    }

    #[test]
    fn it_is_never_empty() {
        assert_eq!(
            Array::try_new(Vec::new(), 0).unwrap_err(),
            BoundedError::EmptyInput
        );
        assert!(serde_json::from_str::<Array>("[]").is_err());
    }

    /// Serde has no context to give the length, so it travels with the items,
    /// in the form of a plain sequence.
    #[test]
    fn serde_keeps_the_length_with_the_items() {
        let array = Array::try_new(vec![1, 2], 2).unwrap();

        let bytes = bincode::serialize(&array).unwrap();

        assert_eq!(bytes, bincode::serialize(&vec![1u8, 2]).unwrap());
        assert_eq!(bincode::deserialize::<Array>(&bytes).unwrap(), array);
    }
}
