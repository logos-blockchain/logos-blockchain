use core::{ops::Deref, slice::Iter};
use std::vec::IntoIter;

use serde::{Deserialize, Deserializer};

use crate::bounded::{
    BoundedError,
    collection::{RuntimeSeqVisitor, collect_iter_up_to},
    runtime::{RuntimeBounded, RuntimeMax},
};

/// `Vec<T>` whose length was checked to be in the range `[MIN, max_len]`,
/// `max_len` given at runtime as a `Max`.
///
/// The runtime counterpart of [`BoundedVec`](crate::bounded::BoundedVec)
/// without its growth and removals, since the vector does not keep its
/// maximum: those are on
/// [`ResizableRuntimeBoundedVec`](crate::bounded::ResizableRuntimeBoundedVec).
/// The checked constructors take the maximum; an empty vector satisfies every
/// maximum, so [`Self::empty`] and `Default` take none.
pub type RuntimeBoundedVec<T, Max, const MIN: usize> = RuntimeBounded<Vec<T>, Max, MIN>;

impl<T, Max, const MIN: usize> RuntimeBounded<Vec<T>, Max, MIN> {
    /// Constructs an empty vector.
    ///
    /// This is only valid when `MIN` is zero, and fails to compile otherwise.
    #[must_use]
    pub const fn empty() -> Self {
        const {
            assert!(
                MIN == 0,
                "Cannot construct empty RuntimeBoundedVec when MIN > 0"
            );
        }
        Self::new_unchecked(Vec::new())
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

    /// Maps borrowed elements while preserving both length bounds.
    #[must_use]
    pub fn map_ref<'a, U, F>(&'a self, f: F) -> RuntimeBoundedVec<U, Max, MIN>
    where
        F: FnMut(&'a T) -> U,
    {
        RuntimeBounded::new_unchecked(self.as_inner().iter().map(f).collect())
    }
}

impl<T, Max, const MIN: usize> RuntimeBounded<Vec<T>, Max, MIN>
where
    Max: RuntimeMax,
{
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
}

impl<T, Max, const MIN: usize> Default for RuntimeBounded<Vec<T>, Max, MIN> {
    fn default() -> Self {
        Self::empty()
    }
}

// Serde reads a vector without a maximum to give, so it holds it to the largest
// `Max` gives.
impl<'de, T, Max, const MIN: usize> Deserialize<'de> for RuntimeBounded<Vec<T>, Max, MIN>
where
    T: Deserialize<'de>,
    Max: RuntimeMax,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(RuntimeSeqVisitor::new())
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

#[cfg(test)]
mod tests {
    use bincode::Options as _;
    use serde::{Deserialize, Deserializer, de::DeserializeOwned};

    use crate::bounded::{BoundedError, RuntimeBoundedVec, UpperRuntimeBoundedVec};

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

    /// Reads `bytes` in the format `bincode::serialize` writes.
    fn from_bincode<T>(bytes: &[u8]) -> bincode::Result<T>
    where
        T: DeserializeOwned,
    {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .allow_trailing_bytes()
            .deserialize(bytes)
    }

    /// A JSON array of `len` ones.
    fn json_array_of(len: usize) -> String {
        format!("[{}]", vec!["1"; len].join(","))
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
    fn try_from_iter_is_stopped_one_item_past_the_maximum() {
        let mut pulled = 0;

        let result = Min0::try_from_iter((0..).inspect(|_| pulled += 1), 4);

        assert_eq!(result, Err(BoundedError::TooManyItems { count: 5, max: 4 }));
        assert_eq!(pulled, 5);
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
    fn deserialization_reads_a_sequence_within_the_bounds_of_the_type() {
        let longest = vec![1u8; usize::from(u8::MAX)];

        let from_json: Min2 = serde_json::from_str(&json_array_of(longest.len())).unwrap();
        let from_bincode: Min2 = from_bincode(&bincode::serialize(&longest).unwrap()).unwrap();

        assert_eq!(from_json.as_slice(), longest);
        assert_eq!(from_bincode.as_slice(), longest);
    }

    #[test]
    fn deserialization_rejects_a_declared_length_past_the_type_before_any_element() {
        let bytes = bincode::serialize(&vec![1u8; usize::from(u8::MAX) + 1]).unwrap();

        let error = from_bincode::<UpperRuntimeBoundedVec<NeverDecoded, u8>>(&bytes)
            .err()
            .unwrap();

        assert!(
            error
                .to_string()
                .contains("Item count 256 exceeds maximum of 255"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialization_holds_a_sequence_without_a_declared_length_to_the_bounds() {
        let too_long = serde_json::from_str::<Min2>(&json_array_of(usize::from(u8::MAX) + 1))
            .unwrap_err()
            .to_string();
        let too_short = serde_json::from_str::<Min2>("[1]").unwrap_err().to_string();

        assert!(
            too_long.contains("Item count 256 exceeds maximum of 255"),
            "unexpected error: {too_long}"
        );
        assert!(
            too_short.contains("Item count 1 is below minimum of 2"),
            "unexpected error: {too_short}"
        );
    }
}
