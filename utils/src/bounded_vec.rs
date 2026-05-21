use core::{
    ops::{Deref, DerefMut},
    slice::Iter,
};
use std::vec::IntoIter;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BoundedError {
    #[error("Input cannot be empty.")]
    EmptyInput,
    #[error("Length {actual} exceeds static maximum of {max}")]
    TooLong { actual: usize, max: usize },
}

/// `Vec<T>` whose length is statically enforced to be in the range `[MIN,
/// MAX]`.
///
/// The invariant is enforced at every construction site (`TryFrom<Vec<T>>`,
/// deserialization), so an instance can never be empty nor have more than `MAX`
/// elements.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    into = "Vec<T>",
    try_from = "Vec<T>",
    bound(serialize = "T: Clone + Serialize")
)]
pub struct BoundedVec<T, const MIN: usize, const MAX: usize>(Vec<T>);

impl<T, const MIN: usize, const MAX: usize> BoundedVec<T, MIN, MAX> {
    pub const MIN: usize = MIN;
    pub const MAX: usize = MAX;

    /// Construct without checking the cap.
    ///
    /// Reserved for callers that have already validated the length. Prefer
    /// [`Self::try_from<Vec<T>>`] at trust boundaries.
    #[must_use]
    pub const fn new_unchecked(items: Vec<T>) -> Self {
        Self(items)
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    // TODO: This function should not return an `Option` when `MIN >= 1`, but at the
    // moment this is not possible in the current Rust version.
    pub fn first(&self) -> Option<&T> {
        self.0.first()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter()
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn try_push(&mut self, item: T) -> Result<(), BoundedError> {
        if self.len() >= MAX {
            return Err(BoundedError::TooLong {
                actual: self.len() + 1,
                max: MAX,
            });
        }
        self.0.push(item);
        Ok(())
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<Vec<T>> for BoundedVec<T, MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        if value.len() < MIN {
            return Err(BoundedError::EmptyInput);
        }
        if value.len() > MAX {
            return Err(BoundedError::TooLong {
                actual: value.len(),
                max: MAX,
            });
        }
        Ok(Self(value))
    }
}

impl<T, const MIN: usize, const MAX: usize> From<T> for BoundedVec<T, MIN, MAX> {
    fn from(value: T) -> Self {
        const { assert!(MIN >= 1, "Min size cannot be zero.") }
        Self([value].into())
    }
}

impl<T, const MIN: usize, const MAX: usize, const INPUT_SIZE: usize> From<[T; INPUT_SIZE]>
    for BoundedVec<T, MIN, MAX>
{
    fn from(value: [T; INPUT_SIZE]) -> Self {
        const { assert!(INPUT_SIZE >= MIN, "Array length is below BoundedVec MIN") }
        const { assert!(INPUT_SIZE <= MAX, "Array length exceeds BoundedVec MAX") }
        Self(value.into())
    }
}

impl<T, const MIN: usize, const MAX: usize, const INPUT_SIZE: usize> From<&[T; INPUT_SIZE]>
    for BoundedVec<T, MIN, MAX>
where
    T: Clone,
{
    fn from(value: &[T; INPUT_SIZE]) -> Self {
        value.clone().into()
    }
}

impl<T, const MIN: usize, const MAX: usize> From<BoundedVec<T, MIN, MAX>> for Vec<T> {
    fn from(value: BoundedVec<T, MIN, MAX>) -> Self {
        value.0
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<[T]> for BoundedVec<T, MIN, MAX> {
    fn as_ref(&self) -> &[T] {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> Deref for BoundedVec<T, MIN, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> DerefMut for BoundedVec<T, MIN, MAX> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<Vec<T>> for BoundedVec<T, MIN, MAX> {
    fn as_ref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<'a, T, const MIN: usize, const MAX: usize> IntoIterator for &'a BoundedVec<T, MIN, MAX> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T, const MIN: usize, const MAX: usize> IntoIterator for BoundedVec<T, MIN, MAX> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// `[0, MAX]` elements.
pub type UpperBoundedVec<T, const MAX: usize> = BoundedVec<T, 0, MAX>;
// `[MIN, usize::MAX]` elements.
pub type LowerBoundedVec<T, const MIN: usize> = BoundedVec<T, MIN, { usize::MAX }>;
// `[1, MAX]` elements.
pub type NonEmptyBoundedVec<T, const MAX: usize> = BoundedVec<T, 1, MAX>;
