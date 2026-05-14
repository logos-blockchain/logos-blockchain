use core::{
    ops::{Deref, DerefMut},
    slice::Iter,
};
use std::vec::IntoIter;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BoundedError {
    #[error("Length {actual} exceeds static maximum of {max}")]
    TooLong { actual: usize, max: usize },
}

/// `Vec<T>` whose length is statically capped at `MAX`. The cap is enforced at
/// every construction site (`new`, `TryFrom<Vec<T>>`, deserialization), so an
/// instance can never hold more than `MAX` elements.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(
    into = "Vec<T>",
    try_from = "Vec<T>",
    bound(serialize = "T: Clone + Serialize")
)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub const MAX: usize = MAX;

    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

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

    pub fn iter(&self) -> Iter<'_, T> {
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
        if self.0.len() >= MAX {
            return Err(BoundedError::TooLong {
                actual: self.0.len() + 1,
                max: MAX,
            });
        }
        self.0.push(item);
        Ok(())
    }
}

impl<T, const MAX: usize> TryFrom<Vec<T>> for BoundedVec<T, MAX> {
    type Error = BoundedError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        if value.len() > MAX {
            return Err(BoundedError::TooLong {
                actual: value.len(),
                max: MAX,
            });
        }
        Ok(Self(value))
    }
}

impl<T, const INPUT_SIZE: usize, const MAX: usize> From<[T; INPUT_SIZE]> for BoundedVec<T, MAX> {
    fn from(value: [T; INPUT_SIZE]) -> Self {
        const { assert!(INPUT_SIZE <= MAX, "Array length exceeds BoundedVec MAX") }
        Self(value.into())
    }
}

impl<T, const INPUT_SIZE: usize, const MAX: usize> From<&[T; INPUT_SIZE]> for BoundedVec<T, MAX>
where
    T: Clone,
{
    fn from(value: &[T; INPUT_SIZE]) -> Self {
        value.clone().into()
    }
}

impl<T, const MAX: usize> From<BoundedVec<T, MAX>> for Vec<T> {
    fn from(value: BoundedVec<T, MAX>) -> Self {
        value.0
    }
}

impl<T, const MAX: usize> AsRef<[T]> for BoundedVec<T, MAX> {
    fn as_ref(&self) -> &[T] {
        &self.0
    }
}

impl<T, const MAX: usize> Deref for BoundedVec<T, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, const MAX: usize> DerefMut for BoundedVec<T, MAX> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T, const MAX: usize> AsRef<Vec<T>> for BoundedVec<T, MAX> {
    fn as_ref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<'a, T, const MAX: usize> IntoIterator for &'a BoundedVec<T, MAX> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T, const MAX: usize> IntoIterator for BoundedVec<T, MAX> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
