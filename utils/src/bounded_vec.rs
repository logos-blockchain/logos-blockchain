use core::{ops::Deref, slice::Iter};
use std::{ops::DerefMut, str::FromStr, vec::IntoIter};

use serde::{Deserialize, Serialize};
use thiserror::Error;

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

    #[must_use]
    pub const fn empty() -> Self {
        const { assert!(MIN == 0, "Cannot construct empty BoundedVec when MIN > 0") }
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
            return Err(BoundedError::TooManyItems {
                count: self.len() + 1,
                max: MAX,
            });
        }
        self.0.push(item);
        Ok(())
    }

    pub fn try_pop(&mut self) -> Result<Option<T>, BoundedError> {
        if self.is_empty() || self.len() - 1 < MIN {
            return Ok(None);
        }

        self.try_remove(self.len() - 1).map(Some)
    }

    pub fn try_remove(&mut self, index: usize) -> Result<T, BoundedError> {
        // This check also guards against an empty vec, `index >= 0` and `self.len() =
        // 0` will return and error
        if index >= self.len() {
            return Err(BoundedError::IndexOutOfBounds {
                index,
                len: self.len(),
            });
        }

        let new_len = self.len() - 1;
        if new_len < MIN {
            return Err(BoundedError::TooFewItems {
                count: new_len,
                min: MIN,
            });
        }

        Ok(self.0.remove(index))
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, T> {
        self.0.iter_mut()
    }
}

impl<T, const MIN: usize, const MAX: usize> Default for BoundedVec<T, MIN, MAX> {
    fn default() -> Self {
        const {
            assert!(
                MIN == 0,
                "Default is only valid for BoundedVec with MIN == 0"
            );
        }
        Self(Vec::new())
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<Vec<T>> for BoundedVec<T, MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        if value.len() < MIN && value.is_empty() {
            return Err(BoundedError::EmptyInput);
        } else if value.len() < MIN {
            return Err(BoundedError::TooFewItems {
                count: value.len(),
                min: MIN,
            });
        }
        if value.len() > MAX {
            return Err(BoundedError::TooManyItems {
                count: value.len(),
                max: MAX,
            });
        }
        Ok(Self(value))
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<&[T]> for BoundedVec<T, MIN, MAX>
where
    T: Clone,
{
    type Error = BoundedError;

    fn try_from(value: &[T]) -> Result<Self, Self::Error> {
        Self::try_from(value.to_vec())
    }
}

impl<T, const MIN: usize, const MAX: usize> From<T> for BoundedVec<T, MIN, MAX> {
    fn from(value: T) -> Self {
        const {
            assert!(
                MIN <= 1,
                "Single-element construction is invalid for minimum bound > 1"
            );
        }
        const {
            assert!(
                MAX >= 1,
                "Single-element construction is invalid for maximum bound < 1"
            );
        }
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

impl<const MAX: usize> FromStr for BoundedVec<u8, 0, MAX> {
    type Err = BoundedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.as_bytes().to_vec())
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

impl<'a, T, const MIN: usize, const MAX: usize> IntoIterator for &'a mut BoundedVec<T, MIN, MAX> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
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
// `[0, usize::MAX]` elements.
pub type MaxBoundedVec<T> = UpperBoundedVec<T, { usize::MAX }>;

#[cfg(test)]
mod tests {
    use crate::bounded_vec::{BoundedError, BoundedVec};

    /// Concrete instantiation used across the tests: between 2 and 4 elements.
    type TestBoundedVectorMin2 = BoundedVec<u8, 2, 4>;
    type TestBoundedVectorMin1 = BoundedVec<u8, 1, 4>;
    type TestBoundedVectorMin0 = BoundedVec<u8, 0, 4>;

    #[test]
    fn from_accepts_single_element_construction() {
        let single = TestBoundedVectorMin0::from(1);
        assert_eq!(single.as_slice(), &[1]);
        let single = TestBoundedVectorMin1::from(1);
        assert_eq!(single.as_slice(), &[1]);

        /*
           This does not compile:
           ```
           let single = TestBoundedVectorMin2::from(1);
           ```
        */
    }

    #[test]
    fn min_max_constants_reflect_the_generic_parameters() {
        assert_eq!(TestBoundedVectorMin2::MIN, 2);
        assert_eq!(TestBoundedVectorMin2::MAX, 4);
    }

    #[test]
    fn new_unchecked_wraps_without_validation() {
        // `new_unchecked` deliberately bypasses the bounds, so it accepts
        // inputs that `try_from` would reject.
        let empty = TestBoundedVectorMin2::new_unchecked(vec![]);
        assert!(empty.is_empty());

        let too_long = TestBoundedVectorMin2::new_unchecked(vec![1, 2, 3, 4, 5]);
        assert_eq!(too_long.len(), 5);
    }

    #[test]
    fn len_and_is_empty() {
        let bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        assert_eq!(bv.len(), 3);
        assert!(!bv.is_empty());

        assert!(TestBoundedVectorMin2::new_unchecked(vec![]).is_empty());
        assert_eq!(TestBoundedVectorMin2::new_unchecked(vec![]).len(), 0);
    }

    #[test]
    fn first_returns_the_leading_element() {
        let bv = TestBoundedVectorMin2::try_from(vec![10, 20, 30]).unwrap();
        assert_eq!(bv.first(), Some(&10));

        assert_eq!(TestBoundedVectorMin2::new_unchecked(vec![]).first(), None);
    }

    #[test]
    fn iter_yields_every_element_in_order() {
        let bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        assert_eq!(bv.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn into_inner_returns_the_backing_vec() {
        let bv = TestBoundedVectorMin2::try_from(vec![7, 8]).unwrap();
        assert_eq!(bv.into_inner(), vec![7, 8]);
    }

    #[test]
    fn as_slice_exposes_the_contents() {
        let bv = TestBoundedVectorMin2::try_from(vec![4, 5, 6]).unwrap();
        assert_eq!(bv.as_slice(), &[4, 5, 6]);
    }

    #[test]
    fn try_push_appends_while_under_the_cap() {
        let mut bv = TestBoundedVectorMin2::try_from(vec![1, 2]).unwrap();
        assert_eq!(bv.try_push(3), Ok(()));
        assert_eq!(bv.try_push(4), Ok(()));
        assert_eq!(bv.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn try_push_rejects_growth_past_max() {
        let mut bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3, 4]).unwrap();
        assert_eq!(
            bv.try_push(5),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        // The failed push must not have mutated the vector.
        assert_eq!(bv.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn try_from_accepts_lengths_within_bounds() {
        TestBoundedVectorMin0::try_from(vec![]).unwrap();
        TestBoundedVectorMin0::try_from(vec![1]).unwrap();
        TestBoundedVectorMin0::try_from(vec![1, 2]).unwrap();
        TestBoundedVectorMin0::try_from(vec![1, 2, 3]).unwrap();
        TestBoundedVectorMin0::try_from(vec![1, 2, 3, 4]).unwrap();

        TestBoundedVectorMin2::try_from(vec![1, 2]).unwrap();
        TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        TestBoundedVectorMin2::try_from(vec![1, 2, 3, 4]).unwrap();
    }

    #[test]
    fn try_from_rejects_input_below_min() {
        assert_eq!(
            TestBoundedVectorMin2::try_from(vec![]),
            Err(BoundedError::EmptyInput)
        );
        assert_eq!(
            TestBoundedVectorMin2::try_from(vec![1]),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
    }

    #[test]
    fn try_from_rejects_input_above_max() {
        assert_eq!(
            TestBoundedVectorMin2::try_from(vec![1, 2, 3, 4, 5]),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(
            TestBoundedVectorMin0::try_from(vec![1, 2, 3, 4, 5]),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
    }

    #[test]
    fn from_single_value_builds_a_one_element_vec() {
        // `From<T>` requires `MIN >= 1`; `MAX` here comfortably allows one.
        let bv: BoundedVec<u8, 1, 4> = 42.into();
        assert_eq!(bv.as_slice(), &[42]);
    }

    #[test]
    fn from_owned_array() {
        let bv: TestBoundedVectorMin2 = [1, 2, 3].into();
        assert_eq!(bv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn from_array_reference() {
        let bv: TestBoundedVectorMin2 = (&[9, 8, 7]).into();
        assert_eq!(bv.as_slice(), &[9, 8, 7]);
    }

    #[test]
    fn into_vec_unwraps_the_bounded_vec() {
        let bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        let raw: Vec<u8> = bv.into();
        assert_eq!(raw, vec![1, 2, 3]);
    }

    #[test]
    fn as_ref_slice_and_vec() {
        let bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        let slice: &[u8] = bv.as_ref();
        assert_eq!(slice, &[1, 2, 3]);
        let vec: &Vec<u8> = bv.as_ref();
        assert_eq!(vec, &vec![1, 2, 3]);
    }

    #[test]
    fn into_iterator_by_reference() {
        let bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        let collected: Vec<u8> = (&bv).into_iter().copied().collect();
        assert_eq!(collected, vec![1, 2, 3]);
        // `bv` is still usable after iterating by reference.
        assert_eq!(bv.len(), 3);
    }

    #[test]
    fn into_iterator_by_value() {
        let bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        let collected: Vec<u8> = bv.into_iter().collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }

    #[test]
    fn equality_and_ordering() {
        let a = TestBoundedVectorMin2::try_from(vec![1, 2]).unwrap();
        let b = TestBoundedVectorMin2::try_from(vec![1, 2]).unwrap();
        let c = TestBoundedVectorMin2::try_from(vec![1, 3]).unwrap();
        assert_eq!(a, b);
        assert!(a < c);
    }

    #[test]
    fn serialize_emits_a_plain_sequence() {
        let bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();
        assert_eq!(serde_json::to_string(&bv).unwrap(), "[1,2,3]");
    }

    #[test]
    fn deserialize_accepts_input_within_bounds() {
        let bv: TestBoundedVectorMin2 = serde_json::from_str("[1,2,3]").unwrap();
        assert_eq!(bv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn serialize_then_deserialize_roundtrips() {
        let original = TestBoundedVectorMin2::try_from(vec![5, 6, 7, 8]).unwrap();
        let json = serde_json::to_string(&original).unwrap();
        let restored: TestBoundedVectorMin2 = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn deserialize_rejects_input_below_min() {
        let err = serde_json::from_str::<TestBoundedVectorMin2>("[1]").unwrap_err();
        assert!(
            err.to_string()
                .contains("Item count 1 is below minimum of 2"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_empty_input() {
        let err = serde_json::from_str::<TestBoundedVectorMin2>("[]").unwrap_err();
        assert!(
            err.to_string().contains("Input cannot be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_input_above_max() {
        let err = serde_json::from_str::<TestBoundedVectorMin2>("[1,2,3,4,5]").unwrap_err();
        assert!(
            err.to_string().contains("exceeds static maximum"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn try_pop_returns_none_at_or_below_lower_bound_and_is_idempotent() {
        let mut bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();

        assert_eq!(bv.try_pop(), Ok(Some(3)));
        assert_eq!(bv.try_pop(), Ok(None));
        assert_eq!(bv.try_pop(), Ok(None));

        assert_eq!(bv.as_slice(), &[1, 2]);
        assert_eq!(bv.len(), 2);

        let mut bv = TestBoundedVectorMin0::try_from(vec![1, 2, 3]).unwrap();

        assert_eq!(bv.try_pop(), Ok(Some(3)));
        assert_eq!(bv.try_pop(), Ok(Some(2)));
        assert_eq!(bv.try_pop(), Ok(Some(1)));
        assert_eq!(bv.try_pop(), Ok(None));
        assert_eq!(bv.try_pop(), Ok(None));

        assert!(bv.is_empty());
    }

    #[test]
    fn try_remove_removes_item_at_index_when_above_min() {
        let mut bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3, 4]).unwrap();

        assert_eq!(bv.try_remove(1), Ok(2));
        assert_eq!(bv.as_slice(), &[1, 3, 4]);
        assert_eq!(bv.len(), 3);
    }

    #[test]
    fn try_remove_rejects_removal_below_min_and_does_not_mutate() {
        let mut bv = TestBoundedVectorMin2::try_from(vec![1, 2]).unwrap();

        assert_eq!(
            bv.try_remove(0),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );

        assert_eq!(bv.as_slice(), &[1, 2]);
        assert_eq!(bv.len(), 2);
    }

    #[test]
    fn try_remove_rejects_out_of_bounds_and_does_not_mutate() {
        let mut bv = TestBoundedVectorMin2::try_from(vec![1, 2, 3]).unwrap();

        assert_eq!(
            bv.try_remove(3),
            Err(BoundedError::IndexOutOfBounds { index: 3, len: 3 })
        );

        assert_eq!(bv.as_slice(), &[1, 2, 3]);
        assert_eq!(bv.len(), 3);
    }
}
