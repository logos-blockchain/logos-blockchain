use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    slice::{Iter, IterMut},
};
use std::vec::IntoIter;

use serde::{Deserialize, Deserializer, de::DeserializeSeed};

use crate::bounded::{
    BoundedError,
    collection::{RuntimeSeqVisitor, collect_iter_up_to},
    runtime::RuntimeBounded,
};

/// `Vec<T>` whose length is enforced to be in the range `[MIN, max_len]`,
/// `max_len` given at runtime as a `Max`.
///
/// The runtime counterpart of [`BoundedVec`](crate::bounded::BoundedVec), with
/// the same operations: the constructors take the maximum, and the value keeps
/// it for every later growth. `BoundedVec`'s constructors that take no
/// maximum (`Default`, the `From` impls, `TryFrom<Vec<T>>`) have no
/// counterpart, since the maximum has to come from somewhere: use
/// [`RuntimeBounded::try_new`].
pub type RuntimeBoundedVec<T, Max, const MIN: usize> = RuntimeBounded<Vec<T>, Max, MIN>;

impl<T, Max, const MIN: usize> RuntimeBounded<Vec<T>, Max, MIN> {
    /// Constructs an empty vector bounded by `max_len`.
    ///
    /// This is only valid when `MIN` is zero, and fails to compile otherwise.
    #[must_use]
    pub const fn empty(max_len: Max) -> Self {
        const {
            assert!(
                MIN == 0,
                "Cannot construct empty RuntimeBoundedVec when MIN > 0"
            );
        }
        Self::new_unchecked(Vec::new(), max_len)
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

    /// Removes and returns the last element if the minimum length is kept.
    ///
    /// Returns `Ok(None)` when the vector is empty or already at its minimum
    /// length.
    pub fn try_pop(&mut self) -> Result<Option<T>, BoundedError> {
        if self.is_empty() || self.len() == MIN {
            return Ok(None);
        }

        // `-1` will never underflow because we already checked that the length is
        // greater than MIN.
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

        if self.len() == MIN {
            return Err(BoundedError::TooFewItems {
                // `-1` will never underflow because we checked above that the index is >= than our
                // length, which must be grater than `0`.
                count: self.len() - 1,
                min: MIN,
            });
        }

        Ok(self.inner.remove(index))
    }

    /// Returns a mutable iterator over the elements of the vector.
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        self.inner.iter_mut()
    }
}

impl<T, Max, const MIN: usize> RuntimeBounded<Vec<T>, Max, MIN>
where
    Max: Copy + Into<usize>,
{
    /// Constructs an empty vector bounded by `max_len`, with at least the
    /// specified capacity.
    ///
    /// The `capacity` must be less than `max_len`; returns
    /// [`BoundedError::CapacityOutOfBounds`] when `capacity` is outside this
    /// range.
    ///
    /// This does **not** change the length of the vector (it is still zero
    /// after construction), but pre-allocates space so that at least
    /// `capacity` elements can be pushed without reallocation.
    pub fn with_capacity(capacity: usize, max_len: Max) -> Result<Self, BoundedError> {
        const {
            assert!(
                MIN == 0,
                "Cannot construct with given capacity when MIN > 0"
            );
        }
        let max = max_len.into();
        if capacity > max {
            return Err(BoundedError::TooManyItems {
                count: capacity,
                max,
            });
        }
        Ok(Self::new_unchecked(Vec::with_capacity(capacity), max_len))
    }

    /// Appends an element if doing so does not exceed the maximum.
    ///
    /// Returns [`BoundedError::TooManyItems`] when the vector is already at
    /// its maximum length.
    pub fn try_push(&mut self, item: T) -> Result<(), BoundedError> {
        let max = self.max_len.into();
        if self.len() >= max {
            return Err(BoundedError::TooManyItems {
                count: self.len() + 1,
                max,
            });
        }
        self.inner.push(item);
        Ok(())
    }

    /// Constructs a vector bounded by `max_len` from an iterable of elements.
    ///
    /// Returns an error if the iterable contains fewer than `MIN` or more than
    /// `max_len` elements. Iteration stops as soon as the maximum is exceeded.
    pub fn try_from_iter<I>(iterable: I, max_len: Max) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = T>,
    {
        collect_iter_up_to(iterable, max_len)
    }

    /// Maps borrowed elements while preserving both length bounds.
    #[must_use]
    pub fn map_ref<'a, U, F>(&'a self, f: F) -> RuntimeBoundedVec<U, Max, MIN>
    where
        F: FnMut(&'a T) -> U,
    {
        RuntimeBounded::new_unchecked(self.as_inner().iter().map(f).collect(), self.max_len)
    }
}

impl<T, Max, const MIN: usize> From<RuntimeBounded<Self, Max, MIN>> for Vec<T> {
    fn from(value: RuntimeBounded<Self, Max, MIN>) -> Self {
        value.into_inner()
    }
}

impl<T, Max, const MIN: usize> AsRef<[T]> for RuntimeBounded<Vec<T>, Max, MIN> {
    fn as_ref(&self) -> &[T] {
        self.as_inner()
    }
}

impl<T, Max, const MIN: usize> Deref for RuntimeBounded<Vec<T>, Max, MIN> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<T, Max, const MIN: usize> DerefMut for RuntimeBounded<Vec<T>, Max, MIN> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<'a, T, Max, const MIN: usize> IntoIterator for &'a mut RuntimeBounded<Vec<T>, Max, MIN> {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter_mut()
    }
}

impl<'a, T, Max, const MIN: usize> IntoIterator for &'a RuntimeBounded<Vec<T>, Max, MIN> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_inner().iter()
    }
}

impl<T, Max, const MIN: usize> IntoIterator for RuntimeBounded<Vec<T>, Max, MIN> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

/// A runtime-bounded vector containing between zero and its maximum elements.
pub type UpperRuntimeBoundedVec<T, Max> = RuntimeBoundedVec<T, Max, 0>;
/// A non-empty runtime-bounded vector containing at most its maximum elements.
pub type NonEmptyRuntimeBoundedVec<T, Max> = RuntimeBoundedVec<T, Max, 1>;

/// Deserializes a sequence into a [`RuntimeBoundedVec`] bounded by the
/// maximum the seed carries.
///
/// `Deserialize` has nowhere to learn a maximum from, so whoever knows it hands
/// it over in a seed. As for a `BoundedVec`, a declared length outside
/// `[MIN, max_len]` is rejected before any element is decoded, and formats that
/// declare nothing are held to the same bounds as elements arrive.
pub struct RuntimeBoundedVecSeed<T, Max, const MIN: usize> {
    max_len: Max,
    items: PhantomData<T>,
}

impl<T, Max, const MIN: usize> RuntimeBoundedVecSeed<T, Max, MIN> {
    #[must_use]
    pub const fn new(max_len: Max) -> Self {
        Self {
            max_len,
            items: PhantomData,
        }
    }
}

impl<'de, T, Max, const MIN: usize> DeserializeSeed<'de> for RuntimeBoundedVecSeed<T, Max, MIN>
where
    T: Deserialize<'de>,
    Max: Copy + Into<usize>,
{
    type Value = RuntimeBoundedVec<T, Max, MIN>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(RuntimeSeqVisitor::new(self.max_len))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cmp::Ordering,
        hash::{DefaultHasher, Hash, Hasher as _},
    };

    use bincode::Options as _;
    use serde::{Deserialize, Deserializer, de::DeserializeSeed as _};

    use crate::bounded::{
        BoundedError, RuntimeBoundedVec, RuntimeBoundedVecSeed, UpperRuntimeBoundedVec,
    };

    /// Between 2 elements and a `u8` maximum given at runtime.
    type Min2 = RuntimeBoundedVec<u8, u8, 2>;
    type Min0 = UpperRuntimeBoundedVec<u8, u8>;

    /// An element whose decoding fails the test: a bound that holds must stop
    /// the input before any element is decoded.
    struct NeverDecoded;

    impl<'de> Deserialize<'de> for NeverDecoded {
        fn deserialize<D: Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
            panic!("an element was decoded");
        }
    }

    /// Reads `bytes` with a seed, in the format `bincode::serialize` writes.
    fn from_bincode<T, const MIN: usize>(
        seed: RuntimeBoundedVecSeed<T, u8, MIN>,
        bytes: &[u8],
    ) -> bincode::Result<RuntimeBoundedVec<T, u8, MIN>>
    where
        T: for<'de> Deserialize<'de>,
    {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .allow_trailing_bytes()
            .deserialize_seed(seed, bytes)
    }

    fn from_json<T, const MIN: usize>(
        seed: RuntimeBoundedVecSeed<T, u8, MIN>,
        json: &str,
    ) -> serde_json::Result<RuntimeBoundedVec<T, u8, MIN>>
    where
        T: for<'de> Deserialize<'de>,
    {
        seed.deserialize(&mut serde_json::Deserializer::from_str(json))
    }

    fn hash_of(value: &impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn try_new_checks_the_length_against_both_bounds() {
        assert_eq!(
            Min2::try_new(vec![1, 2, 3, 4], 4).unwrap().as_slice(),
            &[1, 2, 3, 4]
        );
        assert_eq!(Min2::try_new(vec![], 4), Err(BoundedError::EmptyInput));
        assert_eq!(
            Min2::try_new(vec![1], 4),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(
            Min2::try_new(vec![1, 2, 3, 4, 5], 4),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
    }

    #[test]
    fn values_of_one_type_carry_their_own_maximum() {
        let under_two = Min0::try_new(vec![1, 2], 2).unwrap();
        let under_four = Min0::try_new(vec![1, 2], 4).unwrap();

        assert_eq!(under_two.max_len(), 2);
        assert_eq!(under_four.max_len(), 4);
        assert_eq!(
            Min0::try_new(vec![1, 2, 3], 2),
            Err(BoundedError::TooManyItems { count: 3, max: 2 })
        );
    }

    #[test]
    fn try_push_rejects_growth_past_the_maximum() {
        let mut bounded = Min0::try_new(vec![1], 2).unwrap();

        assert_eq!(bounded.try_push(2), Ok(()));
        assert_eq!(
            bounded.try_push(3),
            Err(BoundedError::TooManyItems { count: 3, max: 2 })
        );
        // The failed push must not have mutated the vector.
        assert_eq!(bounded.as_slice(), &[1, 2]);
    }

    #[test]
    fn removals_keep_the_minimum() {
        let mut bounded = Min2::try_new(vec![1, 2, 3], 4).unwrap();

        assert_eq!(bounded.try_pop(), Ok(Some(3)));
        assert_eq!(bounded.try_pop(), Ok(None));
        assert_eq!(
            bounded.try_remove(0),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(
            bounded.try_remove(2),
            Err(BoundedError::IndexOutOfBounds { index: 2, len: 2 })
        );
        assert_eq!(bounded.as_slice(), &[1, 2]);
    }

    #[test]
    fn try_from_iter_is_stopped_one_item_past_the_maximum() {
        let mut pulled = 0;

        let result = Min0::try_from_iter((0..).inspect(|_| pulled += 1), 4);

        assert_eq!(result, Err(BoundedError::TooManyItems { count: 5, max: 4 }));
        assert_eq!(pulled, 5);
    }

    /// The maximum is a constraint on a value, not part of it.
    #[test]
    fn equality_ordering_and_hashing_ignore_the_maximum() {
        let under_four = Min0::try_new(vec![1, 2], 4).unwrap();
        let under_eight = Min0::try_new(vec![1, 2], 8).unwrap();

        assert_eq!(under_four, under_eight);
        assert_eq!(under_four.cmp(&under_eight), Ordering::Equal);
        assert_eq!(hash_of(&under_four), hash_of(&under_eight));
    }

    #[test]
    fn serialization_writes_the_elements_only() {
        let bounded = Min0::try_new(vec![1, 2, 3], 4).unwrap();

        assert_eq!(serde_json::to_string(&bounded).unwrap(), "[1,2,3]");
        assert_eq!(
            bincode::serialize(&bounded).unwrap(),
            bincode::serialize(&vec![1u8, 2, 3]).unwrap()
        );
    }

    #[test]
    fn the_seed_reads_a_sequence_within_its_bounds() {
        let bytes = bincode::serialize(&vec![1u8, 2, 3]).unwrap();

        let from_json = from_json(RuntimeBoundedVecSeed::<u8, u8, 2>::new(3), "[1,2,3]").unwrap();
        let from_bincode =
            from_bincode(RuntimeBoundedVecSeed::<u8, u8, 2>::new(3), &bytes).unwrap();

        assert_eq!(from_json.as_slice(), &[1, 2, 3]);
        assert_eq!(from_json.max_len(), 3);
        assert_eq!(from_bincode.as_slice(), &[1, 2, 3]);
        assert_eq!(from_bincode.max_len(), 3);
    }

    #[test]
    fn the_seed_rejects_a_declared_length_past_its_maximum_before_any_element() {
        let bytes = bincode::serialize(&vec![1u8; 5]).unwrap();

        let error = from_bincode(RuntimeBoundedVecSeed::<NeverDecoded, u8, 0>::new(4), &bytes)
            .err()
            .unwrap();

        assert!(
            error
                .to_string()
                .contains("Item count 5 exceeds maximum of 4"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn the_seed_holds_a_sequence_without_a_declared_length_to_its_bounds() {
        let too_long = from_json(RuntimeBoundedVecSeed::<u8, u8, 2>::new(4), "[1,2,3,4,5]")
            .unwrap_err()
            .to_string();
        let too_short = from_json(RuntimeBoundedVecSeed::<u8, u8, 2>::new(4), "[1]")
            .unwrap_err()
            .to_string();

        assert!(
            too_long.contains("Item count 5 exceeds maximum of 4"),
            "unexpected error: {too_long}"
        );
        assert!(
            too_short.contains("Item count 1 is below minimum of 2"),
            "unexpected error: {too_short}"
        );
    }
}
