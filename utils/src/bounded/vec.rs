use core::{
    ops::Deref,
    slice::{Iter, IterMut},
};
use std::{ops::DerefMut, str::FromStr, vec::IntoIter};

use crate::bounded::{Bounded, BoundedError, BoundedLen};

impl<T> BoundedLen for Vec<T> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

/// `Vec<T>` whose length is statically enforced to be in the range `[MIN,
/// MAX]`.
///
/// A thin alias over [`Bounded`]: the length checking, (de)serialization and
/// construction machinery lives on the generic wrapper, while the operations
/// below are the ones that only make sense for a `Vec`.
///
/// The invariant is enforced at every checked construction site
/// ([`TryFrom<Vec<T>>`](Self::try_from), deserialization), so an instance can
/// never be shorter than `MIN` nor longer than `MAX`.
pub type BoundedVec<T, const MIN: usize, const MAX: usize> = Bounded<Vec<T>, MIN, MAX>;

impl<T, const MIN: usize, const MAX: usize> Bounded<Vec<T>, MIN, MAX> {
    /// Constructs an empty vector.
    ///
    /// This is only valid when `MIN` is zero; it panics if `MIN` is non-zero.
    #[must_use]
    pub const fn empty() -> Self {
        const { assert!(MIN == 0, "Cannot construct empty BoundedVec when MIN > 0") }
        Self::new_unchecked(Vec::new())
    }

    /// Constructs an empty vector with at least the specified capacity.
    ///
    /// The `capacity` must be within the `[MIN, MAX]` bounds; returns
    /// [`BoundedError::TooFewItems`] when `capacity < MIN` and
    /// [`BoundedError::TooManyItems`] when `capacity > MAX`.
    ///
    /// This does **not** change the length of the vector (it is still zero
    /// after construction), but pre-allocates space so that at least
    /// `capacity` elements can be pushed without reallocation.
    pub fn with_capacity(capacity: usize) -> Result<Self, BoundedError> {
        if capacity < MIN {
            return Err(BoundedError::TooFewItems { count: capacity, min: MIN });
        }
        if capacity > MAX {
            return Err(BoundedError::TooManyItems { count: capacity, max: MAX });
        }
        Ok(Self::new_unchecked(Vec::with_capacity(capacity)))
    }

    /// Returns the number of elements in the vector.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.as_inner().len()
    }

    /// Returns `true` if the vector contains no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.as_inner().is_empty()
    }

    /// Returns the first element, or `None` if the vector is empty.
    #[must_use]
    // TODO: This function should not return an `Option` when `MIN >= 1`, but at the
    // moment this is not possible in the current Rust version.
    pub fn first(&self) -> Option<&T> {
        self.as_inner().first()
    }

    /// Returns an iterator over the elements of the vector.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.as_inner().iter()
    }

    /// Returns the elements as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self.as_inner()
    }

    /// Appends an element if doing so does not exceed `MAX`.
    ///
    /// Returns [`BoundedError::TooManyItems`] when the vector is already at
    /// its maximum length.
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

    /// Removes and returns the last element if the minimum length is kept.
    ///
    /// Returns `Ok(None)` when the vector is empty or already at its minimum
    /// length.
    pub fn try_pop(&mut self) -> Result<Option<T>, BoundedError> {
        if self.is_empty() || self.len() - 1 < MIN {
            return Ok(None);
        }

        self.try_remove(self.len() - 1).map(Some)
    }

    /// Removes and returns the element at `index` if the minimum length is
    /// kept.
    ///
    /// Returns [`BoundedError::IndexOutOfBounds`] for an invalid index and
    /// [`BoundedError::TooFewItems`] when removing the element would violate
    /// `MIN`.
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

    /// Returns a mutable iterator over the elements of the vector.
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        self.0.iter_mut()
    }

    /// Constructs a bounded vector from an iterable of elements.
    ///
    /// Returns an error if the iterable contains fewer than `MIN` or more than
    /// `MAX` elements. Iteration stops as soon as the maximum is exceeded.
    pub fn try_from_iter<I>(iterable: I) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = T>,
    {
        let mut values = Vec::new();

        for value in iterable {
            if values.len() == MAX {
                return Err(BoundedError::TooManyItems {
                    count: MAX + 1,
                    max: MAX,
                });
            }
            values.push(value);
        }
        Self::try_from(values)
    }

    /// Filters and maps elements while preserving the upper length bound.
    ///
    /// Filtering may reduce the collection below `MIN`, so the result has
    /// only an upper bound.
    #[must_use]
    pub fn filter_map_ref<U, F>(&self, f: F) -> UpperBoundedVec<U, MAX>
    where
        F: FnMut(&T) -> Option<U>,
    {
        Bounded::new_unchecked(self.as_inner().iter().filter_map(f).collect())
    }

    /// Maps borrowed elements while preserving both length bounds.
    #[must_use]
    pub fn map_ref<U, F>(&self, f: F) -> BoundedVec<U, MIN, MAX>
    where
        F: FnMut(&T) -> U,
    {
        Bounded::new_unchecked(self.as_inner().iter().map(f).collect())
    }
}

impl<T, const MIN: usize, const MAX: usize> Default for Bounded<Vec<T>, MIN, MAX> {
    fn default() -> Self {
        const {
            assert!(
                MIN == 0,
                "Default is only valid for BoundedVec with MIN == 0"
            );
        }
        Self::new_unchecked(Vec::new())
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<Vec<T>> for Bounded<Vec<T>, MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<&[T]> for Bounded<Vec<T>, MIN, MAX>
where
    T: Clone,
{
    type Error = BoundedError;

    fn try_from(value: &[T]) -> Result<Self, Self::Error> {
        Self::try_from(value.to_vec())
    }
}

impl<T, const MIN: usize, const MAX: usize> From<T> for Bounded<Vec<T>, MIN, MAX> {
    fn from(value: T) -> Self {
        const {
            assert!(
                MIN <= 1,
                "Single-element construction is invalid for minimum bound > 1"
            );
            assert!(
                MAX >= 1,
                "Single-element construction is invalid for maximum bound < 1"
            );
        }
        Self::new_unchecked([value].into())
    }
}

impl<T, const MIN: usize, const MAX: usize, const INPUT_SIZE: usize> From<[T; INPUT_SIZE]>
    for Bounded<Vec<T>, MIN, MAX>
{
    fn from(value: [T; INPUT_SIZE]) -> Self {
        const {
            assert!(INPUT_SIZE >= MIN, "Array length is below BoundedVec MIN");
            assert!(INPUT_SIZE <= MAX, "Array length exceeds BoundedVec MAX");
        }
        Self::new_unchecked(value.into())
    }
}

impl<const MAX: usize> FromStr for Bounded<Vec<u8>, 0, MAX> {
    type Err = BoundedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.as_bytes().to_vec())
    }
}

impl<T, const MIN: usize, const MAX: usize, const INPUT_SIZE: usize> From<&[T; INPUT_SIZE]>
    for Bounded<Vec<T>, MIN, MAX>
where
    T: Clone,
{
    fn from(value: &[T; INPUT_SIZE]) -> Self {
        value.clone().into()
    }
}

impl<T, const MIN: usize, const MAX: usize> From<Bounded<Self, MIN, MAX>> for Vec<T> {
    fn from(value: Bounded<Self, MIN, MAX>) -> Self {
        value.into_inner()
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<[T]> for Bounded<Vec<T>, MIN, MAX> {
    fn as_ref(&self) -> &[T] {
        self.as_inner()
    }
}

impl<T, const MIN: usize, const MAX: usize> Deref for Bounded<Vec<T>, MIN, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<T, const MIN: usize, const MAX: usize> DerefMut for Bounded<Vec<T>, MIN, MAX> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a, T, const MIN: usize, const MAX: usize> IntoIterator for &'a mut Bounded<Vec<T>, MIN, MAX> {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<'a, T, const MIN: usize, const MAX: usize> IntoIterator for &'a Bounded<Vec<T>, MIN, MAX> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_inner().iter()
    }
}

impl<T, const MIN: usize, const MAX: usize> IntoIterator for Bounded<Vec<T>, MIN, MAX> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

/// A bounded vector containing between zero and `MAX` elements.
pub type UpperBoundedVec<T, const MAX: usize> = BoundedVec<T, 0, MAX>;
/// A bounded vector containing at least `MIN` elements.
pub type LowerBoundedVec<T, const MIN: usize> = BoundedVec<T, MIN, { usize::MAX }>;
/// A non-empty bounded vector containing at most `MAX` elements.
pub type NonEmptyBoundedVec<T, const MAX: usize> = BoundedVec<T, 1, MAX>;
/// A vector with no practical length bound.
pub type MaxBoundedVec<T> = UpperBoundedVec<T, { usize::MAX }>;

#[cfg(test)]
mod tests {
    use crate::bounded::{BoundedError, BoundedVec, UpperBoundedVec};

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
        // `From<T>` requires `MIN <= 1`; `MAX` here comfortably allows one.
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

    #[test]
    fn try_from_iter_accepts_items_within_bounds() {
        let bounded = TestBoundedVectorMin0::try_from_iter([1, 2, 3]).unwrap();

        assert_eq!(bounded.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn try_from_iter_accepts_an_empty_iterator_when_minimum_is_zero() {
        let bounded = TestBoundedVectorMin0::try_from_iter(std::iter::empty()).unwrap();

        assert!(bounded.is_empty());
    }

    #[test]
    fn try_from_iter_rejects_items_above_maximum() {
        let result = TestBoundedVectorMin0::try_from_iter([1, 2, 3, 4, 5]);

        assert_eq!(result, Err(BoundedError::TooManyItems { count: 5, max: 4 }));
    }

    #[test]
    fn try_from_iter_rejects_items_below_minimum() {
        let result = TestBoundedVectorMin2::try_from_iter([1]);

        assert_eq!(result, Err(BoundedError::TooFewItems { count: 1, min: 2 }));
    }

    #[test]
    fn try_from_iter_preserves_iterator_order() {
        let input = (0..4).map(|value| value * 2);
        let bounded = TestBoundedVectorMin0::try_from_iter(input).unwrap();

        assert_eq!(bounded.as_slice(), &[0, 2, 4, 6]);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct MyNewType(u32);

    #[test]
    fn map_preserves_bounds_when_mapping_to_another_inner_type() {
        type Source = BoundedVec<u16, 2, 4>;
        type Mapped = BoundedVec<MyNewType, 2, 4>;

        let source = Source::try_from(vec![10u16, 20, 30, 40]).unwrap();

        // The explicit type annotation proves at compile time that map preserves
        // the source bounds while changing the inner type.
        let mapped: Mapped = source.map_ref(|&value| MyNewType(u32::from(value)));

        assert_eq!(Mapped::MIN, Source::MIN);
        assert_eq!(Mapped::MAX, Source::MAX);
        assert_eq!(mapped.len(), 4);
        assert_eq!(
            mapped.as_slice(),
            &[MyNewType(10), MyNewType(20), MyNewType(30), MyNewType(40),]
        );
    }

    #[test]
    fn filter_map_preserves_upper_bound_when_filtering_items() {
        type Source1 = BoundedVec<u16, 2, 4>;
        type Source2 = UpperBoundedVec<u16, 4>;
        type Mapped = UpperBoundedVec<MyNewType, 4>;

        let source_1 = Source1::try_from(vec![10u16, 20, 30, 40]).unwrap();
        let source_2 = Source2::try_from(vec![10u16, 20, 30, 40]).unwrap();

        // The explicit type annotation proves at compile time that filter_map
        // preserves the upper bound while dropping the lower bound.

        let mapped_1: Mapped =
            source_1.filter_map_ref(|value| (*value > 20).then(|| MyNewType(u32::from(*value))));

        assert_eq!(Mapped::MIN, 0);
        assert_eq!(Mapped::MAX, Source1::MAX);
        assert_eq!(mapped_1.len(), 2);
        assert_eq!(mapped_1.as_slice(), &[MyNewType(30), MyNewType(40)]);

        let mapped_2: Mapped =
            source_2.filter_map_ref(|value| (*value > 20).then(|| MyNewType(u32::from(*value))));

        assert_eq!(Mapped::MIN, 0);
        assert_eq!(Mapped::MAX, Source1::MAX);
        assert_eq!(mapped_2.len(), 2);
        assert_eq!(mapped_2.as_slice(), &[MyNewType(30), MyNewType(40)]);
    }

    #[test]
    fn map_ref_preserves_bounds_when_mapping_to_another_inner_type() {
        type Source = BoundedVec<u16, 2, 4>;
        type Mapped = BoundedVec<MyNewType, 2, 4>;

        let source = Source::try_from(vec![10u16, 20, 30, 40]).unwrap();

        // The explicit type annotation proves at compile time that map_ref
        // preserves both the minimum and maximum bounds.
        let mapped: Mapped = source.map_ref(|value| MyNewType(u32::from(*value)));

        assert_eq!(Mapped::MIN, Source::MIN);
        assert_eq!(Mapped::MAX, Source::MAX);
        assert_eq!(mapped.len(), source.len());
        assert_eq!(
            mapped.as_slice(),
            &[MyNewType(10), MyNewType(20), MyNewType(30), MyNewType(40),]
        );
    }

    #[test]
    fn with_capacity_within_bounds_succeeds() {
        type V = BoundedVec<u32, 2, 10>;
        let v = V::with_capacity(5).unwrap();
        assert_eq!(v.len(), 0);
        assert!(v.as_inner().capacity() >= 5);
    }

    #[test]
    fn with_capacity_at_max_succeeds() {
        type V = BoundedVec<u32, 0, 4>;
        let v = V::with_capacity(4).unwrap();
        assert_eq!(v.len(), 0);
        assert!(v.as_inner().capacity() >= 4);
    }

    #[test]
    fn with_capacity_above_max_returns_error() {
        type V = BoundedVec<u32, 0, 4>;
        assert!(matches!(
            V::with_capacity(5),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        ));
    }

    #[test]
    fn with_capacity_below_min_returns_error() {
        type V = BoundedVec<u32, 3, 8>;
        assert!(matches!(
            V::with_capacity(2),
            Err(BoundedError::TooFewItems { count: 2, min: 3 })
        ));
    }

}
