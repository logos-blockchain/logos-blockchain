use core::slice::Iter;
use std::vec::IntoIter;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bounded_vec::{BoundedError, BoundedVec};

pub trait ElementSize {
    fn element_size(&self) -> usize;
}

#[derive(Debug, Error, Eq, PartialEq, Clone)]
pub enum StorageBoundedError {
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
    #[error("Total storage size {size} exceeds maximum of {max} bytes")]
    ContentTooBig { size: usize, max: usize },
    #[error("Removal of an element resulted in a size less than 0 bytes")]
    SizeUnderflow,
}

/// Static bounds for a [`StorageBoundedVec`].
pub struct StorageBoundedVecBounds {
    /// Minimum number of items (inclusive).
    pub min_count: usize,
    /// Maximum number of items (inclusive).
    pub max_count: usize,
    /// Maximum total storage size in bytes, calculated as the sum of all items.
    pub max_size: usize,
}

/// A vector bounded by both item count and total storage size.
///
/// `StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>` ensures that:
/// - the number of items is in range `[MIN_COUNT, MAX_COUNT]`, and
/// - the total storage size, calculated as the sum of all items'
///   [`ElementSize::storage_size`] values, does not exceed `MAX_SIZE`.
///
/// If `T` is a mutable type and has been mutated out-of-band, the size
/// invariant can be refreshed by calling `try_refresh_total_storage_size`,
/// which recomputes the total from scratch. This is a manual step to
/// acknowledge that out-of-band mutation can break the invariant, while still
/// providing a way to recover if the caller is able to ensure that the mutated
/// sizes are still within bounds. Use with caution, and prefer mutation through
/// checked APIs when possible.
///
/// Checked constructors, deserialization, and checked mutation APIs such as
/// [`Self::try_push`] assist these invariants.
///
/// This type intentionally does not expose mutable slice access, because
/// mutating an existing item could change its storage size and invalidate the
/// cached `total_size` / `MAX_SIZE` invariant. For the same reason, it does not
/// expose an `into_bounded` escape hatch; use [`Self::into_inner`] when the raw
/// `Vec<T>` is explicitly needed.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    into = "Vec<T>",
    try_from = "Vec<T>",
    bound(
        serialize = "T: Clone + Serialize",
        deserialize = "T: Deserialize<'de> + ElementSize"
    )
)]
pub struct StorageBoundedVec<
    T,
    const MIN_COUNT: usize,
    const MAX_COUNT: usize,
    const MAX_SIZE: usize,
> {
    items: BoundedVec<T, MIN_COUNT, MAX_COUNT>,
    storage_size: usize,
}

impl<T, const MIN_COUNT: usize, const MAX_COUNT: usize, const MAX_SIZE: usize>
    StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>
{
    /// Returns the static bounds of the type.
    #[must_use]
    pub const fn bounds() -> StorageBoundedVecBounds {
        StorageBoundedVecBounds {
            min_count: MIN_COUNT,
            max_count: MAX_COUNT,
            max_size: MAX_SIZE,
        }
    }

    /// Construct an empty storage-bounded vector.
    #[must_use]
    pub const fn empty() -> Self {
        const {
            assert!(
                MIN_COUNT == 0,
                "Cannot construct empty StorageBoundedVec when MIN_COUNT > 0"
            );
        }

        Self {
            items: BoundedVec::empty(),
            storage_size: 0,
        }
    }

    /// Try to create from a vector, validating both count and total storage
    /// size.
    pub fn try_from_vec(items: Vec<T>) -> Result<Self, StorageBoundedError>
    where
        T: ElementSize,
    {
        let items = BoundedVec::<T, MIN_COUNT, MAX_COUNT>::try_from(items)?;
        let total_size = Self::validate_total_size(items.as_slice())?;

        Ok(Self {
            items,
            storage_size: total_size,
        })
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn first(&self) -> Option<&T> {
        self.items.as_slice().first()
    }

    pub fn iter(&self) -> Iter<'_, T> {
        self.items.as_slice().iter()
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<T> {
        self.items.into_inner()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self.items.as_slice()
    }

    /// Refresh and return the total storage size of the current contents by
    /// recomputing the total. Do not use in a tight loops. This is useful
    /// if `T` is a mutable type and has been mutated out-of-band.
    pub fn try_refresh_total_storage_size(&mut self) -> Result<usize, StorageBoundedError>
    where
        T: ElementSize,
    {
        let total = Self::validate_total_size(self.items.as_slice())?;
        self.storage_size = total;
        Ok(total)
    }

    #[must_use]
    pub const fn cached_total_storage_size(&self) -> usize {
        self.storage_size
    }

    /// Try to push an item while maintaining both count and size constraints.
    pub fn try_push(&mut self, item: T) -> Result<(), StorageBoundedError>
    where
        T: ElementSize,
    {
        if self.len() >= MAX_COUNT {
            return Err(StorageBoundedError::BoundedError(
                BoundedError::TooManyItems {
                    count: self.len() + 1,
                    max: MAX_COUNT,
                },
            ));
        }

        let item_size = item.element_size();
        let new_total =
            self.storage_size
                .checked_add(item_size)
                .ok_or(StorageBoundedError::ContentTooBig {
                    size: usize::MAX,
                    max: MAX_SIZE,
                })?;

        if new_total > MAX_SIZE {
            return Err(StorageBoundedError::ContentTooBig {
                size: new_total,
                max: MAX_SIZE,
            });
        }

        self.items.try_push(item)?;
        self.storage_size = new_total;

        Ok(())
    }

    pub fn try_pop(&mut self) -> Result<Option<T>, StorageBoundedError>
    where
        T: ElementSize,
    {
        let Some(item) = self.items.try_pop()? else {
            return Ok(None);
        };

        self.storage_size -= item.element_size();
        Ok(Some(item))
    }

    pub fn try_remove(&mut self, index: usize) -> Result<T, StorageBoundedError>
    where
        T: ElementSize,
    {
        let item = self.items.try_remove(index)?;

        self.storage_size = self
            .storage_size
            .checked_sub(item.element_size())
            .ok_or(StorageBoundedError::SizeUnderflow)?;

        Ok(item)
    }

    fn validate_total_size(items: &[T]) -> Result<usize, StorageBoundedError>
    where
        T: ElementSize,
    {
        let mut total = 0usize;

        for item in items {
            total = total.checked_add(item.element_size()).ok_or(
                StorageBoundedError::ContentTooBig {
                    size: usize::MAX,
                    max: MAX_SIZE,
                },
            )?;

            if total > MAX_SIZE {
                return Err(StorageBoundedError::ContentTooBig {
                    size: total,
                    max: MAX_SIZE,
                });
            }
        }

        Ok(total)
    }
}

impl<T, const MIN_COUNT: usize, const MAX_COUNT: usize, const MAX_SIZE: usize> TryFrom<Vec<T>>
    for StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>
where
    T: ElementSize,
{
    type Error = StorageBoundedError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Self::try_from_vec(value)
    }
}

impl<T, const MIN_COUNT: usize, const MAX_COUNT: usize, const MAX_SIZE: usize> TryFrom<&[T]>
    for StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>
where
    T: Clone + ElementSize,
{
    type Error = StorageBoundedError;

    fn try_from(value: &[T]) -> Result<Self, Self::Error> {
        Self::try_from_vec(value.to_vec())
    }
}

impl<T, const MIN_COUNT: usize, const MAX_COUNT: usize, const MAX_SIZE: usize, const N: usize>
    TryFrom<&[T; N]> for StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>
where
    T: Clone + ElementSize,
{
    type Error = StorageBoundedError;

    fn try_from(value: &[T; N]) -> Result<Self, Self::Error> {
        Self::try_from(&value[..])
    }
}

impl<T, const MIN_COUNT: usize, const MAX_COUNT: usize, const MAX_SIZE: usize>
    From<StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>> for Vec<T>
{
    fn from(value: StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>) -> Self {
        value.into_inner()
    }
}

impl<'a, T, const MIN_COUNT: usize, const MAX_COUNT: usize, const MAX_SIZE: usize> IntoIterator
    for &'a StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>
{
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.as_slice().iter()
    }
}

impl<T, const MIN_COUNT: usize, const MAX_COUNT: usize, const MAX_SIZE: usize> IntoIterator
    for StorageBoundedVec<T, MIN_COUNT, MAX_COUNT, MAX_SIZE>
{
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_inner().into_iter()
    }
}

/// `[0, MAX_COUNT]` items with max storage size.
pub type UpperStorageBoundedVec<T, const MAX_COUNT: usize, const MAX_SIZE: usize> =
    StorageBoundedVec<T, 0, MAX_COUNT, MAX_SIZE>;

/// `[MIN_COUNT, usize::MAX]` items with max storage size.
pub type LowerStorageBoundedVec<T, const MIN_COUNT: usize, const MAX_SIZE: usize> =
    StorageBoundedVec<T, MIN_COUNT, { usize::MAX }, MAX_SIZE>;

/// `[1, MAX_COUNT]` non-empty items with max storage size.
pub type NonEmptyStorageBoundedVec<T, const MAX_COUNT: usize, const MAX_SIZE: usize> =
    StorageBoundedVec<T, 1, MAX_COUNT, MAX_SIZE>;

#[cfg(test)]
mod tests {
    // Runtime unit tests cannot assert that an API does not exist. To lock in
    // the absence of `DerefMut`, an `into_bounded` escape hatch, or construction
    // with non-`ElementSize` types, use compile-fail tests such as
    // `trybuild`. Adding public methods that just call `unimplemented!` would
    // still expose those APIs and weaken the type's contract.

    use super::*;

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
    struct TestItem {
        value: u8,
        size: usize,
    }

    impl ElementSize for TestItem {
        fn element_size(&self) -> usize {
            self.size
        }
    }

    /// Type alias for tests: 2-4 items, max 100 bytes.
    type TestStorageBoundedVec = StorageBoundedVec<TestItem, 2, 4, 100>;

    fn item(value: u8, size: usize) -> TestItem {
        TestItem { value, size }
    }

    #[test]
    fn constants_reflect_generic_parameters() {
        assert_eq!(TestStorageBoundedVec::bounds().min_count, 2);
        assert_eq!(TestStorageBoundedVec::bounds().max_count, 4);
        assert_eq!(TestStorageBoundedVec::bounds().max_size, 100);
    }

    #[test]
    fn empty_constructs_when_min_count_is_zero() {
        type EmptyAllowed = StorageBoundedVec<TestItem, 0, 4, 100>;
        let sbv = EmptyAllowed::empty();
        assert!(sbv.is_empty());
        assert_eq!(sbv.len(), 0);
        assert_eq!(sbv.cached_total_storage_size(), 0);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_from_vec_accepts_valid_items() {
        let items = vec![item(1, 20), item(2, 30), item(3, 40)];
        let sbv = TestStorageBoundedVec::try_from_vec(items).unwrap();
        assert_eq!(sbv.len(), 3);
        assert_eq!(sbv.cached_total_storage_size(), 90);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_from_vec_accepts_exact_max_size() {
        let items = vec![item(1, 50), item(2, 50)];
        let sbv = TestStorageBoundedVec::try_from_vec(items).unwrap();
        assert_eq!(sbv.cached_total_storage_size(), 100);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_from_vec_rejects_empty_when_min_is_positive() {
        assert_eq!(
            TestStorageBoundedVec::try_from_vec(vec![]),
            Err(StorageBoundedError::BoundedError(BoundedError::EmptyInput))
        );
    }

    #[test]
    fn try_from_vec_rejects_too_few_items() {
        let items = vec![item(1, 10)];
        assert_eq!(
            TestStorageBoundedVec::try_from_vec(items),
            Err(StorageBoundedError::BoundedError(
                BoundedError::TooFewItems { count: 1, min: 2 }
            ))
        );
    }

    #[test]
    fn try_from_vec_rejects_too_many_items() {
        let items = vec![
            item(1, 10),
            item(2, 10),
            item(3, 10),
            item(4, 10),
            item(5, 10),
        ];
        assert_eq!(
            TestStorageBoundedVec::try_from_vec(items),
            Err(StorageBoundedError::BoundedError(
                BoundedError::TooManyItems { count: 5, max: 4 }
            ))
        );
    }

    #[test]
    fn try_from_vec_rejects_oversized_content() {
        let items = vec![item(1, 50), item(2, 60)];
        assert_eq!(
            TestStorageBoundedVec::try_from_vec(items),
            Err(StorageBoundedError::ContentTooBig {
                size: 110,
                max: 100
            })
        );
    }

    #[test]
    fn try_from_vec_checks_size_with_overflow_protection() {
        type HugeStorageBoundedVec = StorageBoundedVec<TestItem, 2, 4, { usize::MAX }>;
        let items = vec![item(1, usize::MAX), item(2, 1)];
        assert_eq!(
            HugeStorageBoundedVec::try_from_vec(items),
            Err(StorageBoundedError::ContentTooBig {
                size: usize::MAX,
                max: usize::MAX,
            })
        );
    }

    #[test]
    fn try_from_slice_validates_count_and_size() {
        let items = [item(1, 20), item(2, 30)];
        let sbv = TestStorageBoundedVec::try_from(&items[..]).unwrap();
        assert_eq!(sbv.as_slice(), &items);
        assert_eq!(sbv.cached_total_storage_size(), 50);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_from_array_reference_validates_count_and_size() {
        let items = [item(1, 20), item(2, 30)];
        let sbv = TestStorageBoundedVec::try_from(&items).unwrap();
        assert_eq!(sbv.as_slice(), &items);
        assert_eq!(sbv.cached_total_storage_size(), 50);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_push_succeeds_within_bounds_and_updates_cached_size() {
        let items = vec![item(1, 20), item(2, 30)];
        let mut sbv = TestStorageBoundedVec::try_from_vec(items).unwrap();
        assert_eq!(sbv.cached_total_storage_size(), 50);

        assert_eq!(sbv.try_push(item(3, 40)), Ok(()));

        assert_eq!(sbv.len(), 3);
        assert_eq!(sbv.cached_total_storage_size(), 90);

        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_push_rejects_when_size_exceeded_and_does_not_mutate() {
        let items = vec![item(1, 50), item(2, 40)];
        let mut sbv = TestStorageBoundedVec::try_from_vec(items).unwrap();
        assert_eq!(
            sbv.try_push(item(3, 20)),
            Err(StorageBoundedError::ContentTooBig {
                size: 110,
                max: 100
            })
        );
        assert_eq!(sbv.as_slice(), &[item(1, 50), item(2, 40)]);
        assert_eq!(sbv.cached_total_storage_size(), 90);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_push_rejects_when_addition_overflows_and_does_not_mutate() {
        type HugeStorageBoundedVec = StorageBoundedVec<TestItem, 1, 4, { usize::MAX }>;
        let mut sbv = HugeStorageBoundedVec::try_from_vec(vec![item(1, usize::MAX)]).unwrap();
        assert_eq!(
            sbv.try_push(item(2, 1)),
            Err(StorageBoundedError::ContentTooBig {
                size: usize::MAX,
                max: usize::MAX,
            })
        );
        assert_eq!(sbv.as_slice(), &[item(1, usize::MAX)]);
        assert_eq!(sbv.cached_total_storage_size(), usize::MAX);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_push_rejects_when_count_exceeded_and_does_not_mutate() {
        let items = vec![item(1, 10), item(2, 10), item(3, 10), item(4, 10)];
        let mut sbv = TestStorageBoundedVec::try_from_vec(items).unwrap();
        assert_eq!(
            sbv.try_push(item(5, 10)),
            Err(StorageBoundedError::BoundedError(
                BoundedError::TooManyItems { count: 5, max: 4 }
            ))
        );
        assert_eq!(
            sbv.as_slice(),
            &[item(1, 10), item(2, 10), item(3, 10), item(4, 10)]
        );
        assert_eq!(sbv.cached_total_storage_size(), 40);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_pop_updates_cached_total_size() {
        type Sbv = StorageBoundedVec<TestItem, 0, 4, 100>;

        let mut sbv = Sbv::try_from_vec(vec![item(1, 20), item(2, 30), item(3, 40)]).unwrap();
        assert_eq!(sbv.cached_total_storage_size(), 90);

        assert_eq!(sbv.try_pop(), Ok(Some(item(3, 40))));
        assert_eq!(sbv.cached_total_storage_size(), 50);

        assert_eq!(sbv.try_pop(), Ok(Some(item(2, 30))));
        assert_eq!(sbv.cached_total_storage_size(), 20);

        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_remove_updates_cached_total_size() {
        type Sbv = StorageBoundedVec<TestItem, 0, 4, 100>;

        let mut sbv = Sbv::try_from_vec(vec![item(1, 20), item(2, 30), item(3, 40)]).unwrap();
        assert_eq!(sbv.cached_total_storage_size(), 90);

        let removed = sbv.try_remove(1).unwrap();

        assert_eq!(removed, item(2, 30));
        assert_eq!(sbv.as_slice(), &[item(1, 20), item(3, 40)]);
        assert_eq!(sbv.cached_total_storage_size(), 60);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_remove_rejects_when_min_count_would_be_violated_and_does_not_mutate() {
        type Sbv = StorageBoundedVec<TestItem, 2, 4, 100>;

        let mut sbv = Sbv::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();

        assert_eq!(
            sbv.try_remove(0),
            Err(StorageBoundedError::BoundedError(
                BoundedError::TooFewItems { count: 1, min: 2 }
            ))
        );

        assert_eq!(sbv.as_slice(), &[item(1, 20), item(2, 30)]);
        assert_eq!(sbv.cached_total_storage_size(), 50);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn try_remove_rejects_out_of_bounds_and_does_not_mutate() {
        type Sbv = StorageBoundedVec<TestItem, 0, 4, 100>;

        let mut sbv = Sbv::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();

        assert_eq!(
            sbv.try_remove(2),
            Err(StorageBoundedError::BoundedError(
                BoundedError::IndexOutOfBounds { index: 2, len: 2 }
            ))
        );

        assert_eq!(sbv.as_slice(), &[item(1, 20), item(2, 30)]);
        assert_eq!(sbv.cached_total_storage_size(), 50);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn first_returns_the_leading_element() {
        let sbv = TestStorageBoundedVec::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();
        assert_eq!(sbv.first(), Some(&item(1, 20)));
    }

    #[test]
    fn iter_yields_every_element_in_order() {
        let sbv = TestStorageBoundedVec::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();
        let values: Vec<_> = sbv.as_slice().iter().map(|item| item.value).collect();
        assert_eq!(values, vec![1, 2]);
    }

    #[test]
    fn into_inner_unwraps_the_vec() {
        let items = vec![item(1, 20), item(2, 30)];
        let sbv = TestStorageBoundedVec::try_from_vec(items.clone()).unwrap();
        let raw: Vec<_> = sbv.into_inner();
        assert_eq!(raw, items);
    }

    #[test]
    fn deref_provides_read_only_slice_access() {
        let items = vec![item(1, 20), item(2, 30)];
        let sbv = TestStorageBoundedVec::try_from_vec(items).unwrap();
        assert_eq!(sbv.len(), 2);
        assert_eq!(sbv.as_slice()[0].value, 1);
    }

    #[test]
    fn as_ref_slice_and_vec() {
        let items = vec![item(1, 20), item(2, 30)];
        let sbv = TestStorageBoundedVec::try_from_vec(items.clone()).unwrap();
        let slice: &[TestItem] = sbv.as_slice();
        assert_eq!(slice, items.as_slice());
        let vec: Vec<TestItem> = sbv.as_slice().to_vec();
        assert_eq!(vec, items);
    }

    #[test]
    fn into_iterator_by_reference() {
        let sbv = TestStorageBoundedVec::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();
        let collected: Vec<_> = (&sbv).into_iter().copied().collect();
        assert_eq!(collected, vec![item(1, 20), item(2, 30)]);
        assert_eq!(sbv.len(), 2);
    }

    #[test]
    fn into_iterator_by_value() {
        let sbv = TestStorageBoundedVec::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();
        let collected: Vec<_> = sbv.into_iter().collect();
        assert_eq!(collected, vec![item(1, 20), item(2, 30)]);
    }

    #[test]
    fn serialize_emits_a_plain_sequence() {
        let sbv = TestStorageBoundedVec::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();
        assert_eq!(
            serde_json::to_string(&sbv).unwrap(),
            r#"[{"value":1,"size":20},{"value":2,"size":30}]"#
        );
    }

    #[test]
    fn deserialize_accepts_input_within_bounds() {
        let sbv: TestStorageBoundedVec =
            serde_json::from_str(r#"[{"value":1,"size":20},{"value":2,"size":30}]"#).unwrap();
        assert_eq!(sbv.as_slice(), &[item(1, 20), item(2, 30)]);
        assert_eq!(sbv.cached_total_storage_size(), 50);
        let mut sbv_check = sbv.clone();
        assert_eq!(
            sbv.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn serde_roundtrip_recomputes_cached_total_size() {
        let original = TestStorageBoundedVec::try_from_vec(vec![item(1, 20), item(2, 30)]).unwrap();
        let json = serde_json::to_string(&original).unwrap();
        let restored: TestStorageBoundedVec = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.as_slice(), &[item(1, 20), item(2, 30)]);
        assert_eq!(restored.cached_total_storage_size(), 50);
        let mut sbv_check = restored.clone();
        assert_eq!(
            restored.cached_total_storage_size(),
            sbv_check.try_refresh_total_storage_size().unwrap()
        );
    }

    #[test]
    fn serde_rejects_named_struct_shape_with_stale_cached_total() {
        let err = serde_json::from_str::<TestStorageBoundedVec>(
            r#"{"items":[{"value":1,"size":20},{"value":2,"size":30}],"total_size":0}"#,
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("invalid type"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_input_below_min() {
        let err = serde_json::from_str::<TestStorageBoundedVec>(r#"[{"value":1,"size":20}]"#)
            .unwrap_err();
        assert!(
            err.to_string().contains("below minimum"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_input_above_max_count() {
        let err = serde_json::from_str::<TestStorageBoundedVec>(
            r#"[{"value":1,"size":10},{"value":2,"size":10},{"value":3,"size":10},{"value":4,"size":10},{"value":5,"size":10}]"#,
        )
            .unwrap_err();
        assert!(
            err.to_string().contains("exceeds static maximum"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_oversized_content() {
        let err = serde_json::from_str::<TestStorageBoundedVec>(
            r#"[{"value":1,"size":50},{"value":2,"size":60}]"#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Total storage size"),
            "unexpected error: {err}"
        );
    }
}
