use core::{ops::Deref, slice::IterMut};

use crate::bounded::{
    BoundedError, BoundedLen,
    runtime::{RuntimeBounded, RuntimeMax},
};

/// A [`RuntimeBounded`] that keeps the maximum its length was checked against,
/// so that it can grow and shrink within `[MIN, max_len]`.
///
/// Reads go through the checked value it wraps. It has no codec and no serde:
/// once built, it is turned into that value ([`Self::into_bounded`]), which is
/// what gets encoded, stored or sent.
#[derive(Clone, Debug)]
pub struct ResizableRuntimeBounded<T, Max, const MIN: usize> {
    bounded: RuntimeBounded<T, Max, MIN>,
    max_len: Max,
}

impl<T, Max, const MIN: usize> ResizableRuntimeBounded<T, Max, MIN>
where
    T: BoundedLen,
    Max: RuntimeMax,
{
    /// Checked constructor for any [measurable](BoundedLen) `T`.
    pub fn try_new(inner: T, max_len: Max) -> Result<Self, BoundedError> {
        Ok(Self {
            bounded: RuntimeBounded::try_new(inner, max_len)?,
            max_len,
        })
    }
}

impl<T, Max, const MIN: usize> ResizableRuntimeBounded<T, Max, MIN> {
    /// The largest length the value may reach.
    #[must_use]
    pub const fn max_len(&self) -> Max
    where
        Max: Copy,
    {
        self.max_len
    }
}

impl<T, Max, const MIN: usize> From<ResizableRuntimeBounded<T, Max, MIN>>
    for RuntimeBounded<T, Max, MIN>
{
    fn from(value: ResizableRuntimeBounded<T, Max, MIN>) -> Self {
        value.bounded
    }
}

impl<T, Max, const MIN: usize> Deref for ResizableRuntimeBounded<T, Max, MIN> {
    type Target = RuntimeBounded<T, Max, MIN>;

    fn deref(&self) -> &Self::Target {
        &self.bounded
    }
}

/// A runtime-bounded `Vec<T>` that keeps its maximum, to grow and shrink
/// within `[MIN, max_len]`.
pub type ResizableRuntimeBoundedVec<T, Max, const MIN: usize> =
    ResizableRuntimeBounded<Vec<T>, Max, MIN>;

impl<T, Max, const MIN: usize> ResizableRuntimeBounded<Vec<T>, Max, MIN> {
    /// Constructs an empty vector bounded by `max_len`.
    ///
    /// This is only valid when `MIN` is zero, and fails to compile otherwise.
    #[must_use]
    pub const fn empty(max_len: Max) -> Self {
        Self {
            bounded: RuntimeBounded::empty(),
            max_len,
        }
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

        Ok(self.bounded.inner.remove(index))
    }

    /// Returns a mutable iterator over the elements of the vector.
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        self.bounded.inner.iter_mut()
    }
}

impl<T, Max, const MIN: usize> ResizableRuntimeBounded<Vec<T>, Max, MIN>
where
    Max: RuntimeMax,
{
    /// Constructs an empty vector bounded by `max_len`, with at least the
    /// specified capacity.
    ///
    /// The `capacity` must not exceed `max_len`; returns
    /// [`BoundedError::TooManyItems`] when it does.
    ///
    /// This does **not** change the length of the vector (it is still zero
    /// after construction), but pre-allocates space so that at least
    /// `capacity` elements can be pushed without reallocation.
    pub fn with_capacity_and_bound(capacity: usize, max_len: Max) -> Result<Self, BoundedError> {
        const {
            assert!(
                MIN == 0,
                "Cannot construct with given capacity when MIN > 0"
            );
        }
        let max = max_len.to_usize();
        if capacity > max {
            return Err(BoundedError::TooManyItems {
                count: capacity,
                max,
            });
        }
        Ok(Self {
            bounded: RuntimeBounded::new_unchecked(Vec::with_capacity(capacity)),
            max_len,
        })
    }

    /// Appends an element if doing so does not exceed the maximum.
    ///
    /// Returns [`BoundedError::TooManyItems`] when the vector is already at
    /// its maximum length.
    pub fn try_push(&mut self, item: T) -> Result<(), BoundedError> {
        let max = self.max_len.to_usize();
        if self.len() >= max {
            return Err(BoundedError::TooManyItems {
                count: self.len() + 1,
                max,
            });
        }
        self.bounded.inner.push(item);
        Ok(())
    }
}

impl<'a, T, Max, const MIN: usize> IntoIterator
    for &'a mut ResizableRuntimeBounded<Vec<T>, Max, MIN>
{
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use crate::bounded::{BoundedError, ResizableRuntimeBoundedVec};

    #[test]
    fn try_push_rejects_growth_past_the_maximum() {
        let mut bounded = ResizableRuntimeBoundedVec::<u8, u8, 0>::try_new(vec![1], 2).unwrap();

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
        let mut bounded =
            ResizableRuntimeBoundedVec::<u8, u8, 2>::try_new(vec![1, 2, 3], 4).unwrap();

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
}
