//! Building a bounded collection item by item, from an iterator or from a
//! serde sequence or map.
//!
//! Every checked path that builds a [`Bounded`] collection one item at a time
//! goes through [`collect`], so the same rules hold for all of them:
//!
//! - The first `MIN` items are mandatory, like the fields of a struct. An input
//!   that ends before them fails where it ends.
//! - Past `MIN` the input may end at any point, and it is stopped one item past
//!   `MAX`.
//! - A collection may refuse an item it already holds. That is an error, never
//!   a silent merge: merging would let `[a, a]` pass a `MIN = 2` bound that it
//!   does not meet, and would let two different inputs produce the same value.
//! - A declared length is trusted for pre-allocation only up to the budget
//!   every bounded collection shares (see [`allocation_size_for_hint`]).
//!
//! Each bound is checked as the items arrive, so wrapping the finished
//! collection needs no check at all. Deserialization also rejects a declared
//! length outside the bounds before a single item is decoded.

use core::{convert::identity, fmt, marker::PhantomData};

use serde::{
    Deserialize,
    de::{Error as _, SeqAccess, Visitor},
};

use crate::bounded::{Bounded, BoundedError, allocation_size_for_hint};

/// A collection a bounded type can be built from, one item at a time.
pub trait BoundedCollection: Sized {
    /// What one insertion adds.
    type Item;

    fn with_capacity(capacity: usize) -> Self;

    /// Adds `item`, unless the collection already holds it, in which case the
    /// collection is left untouched and `false` is returned. A vector accepts
    /// every item.
    fn add(&mut self, item: Self::Item) -> bool;
}

/// Builds a bounded collection from the items `next` yields until it returns
/// `None`.
///
/// `next` is given the collection built so far, so a source that reads an item
/// in parts can refuse it before reading the rest, as the ordered map's visitor
/// does with a key it cannot admit. Whatever `next` yields is still checked
/// here like any other item.
///
/// `hint` is the length the input declares, if any, and only sizes the initial
/// allocation. `into_error` turns a bound violation into the input's own error
/// type.
pub fn collect<Collection, VisitorFn, ErrorFn, Error, const MIN: usize, const MAX: usize>(
    hint: Option<usize>,
    mut next: VisitorFn,
    into_error: ErrorFn,
) -> Result<Bounded<Collection, MIN, MAX>, Error>
where
    Collection: BoundedCollection,
    VisitorFn: FnMut(&Collection) -> Result<Option<Collection::Item>, Error>,
    ErrorFn: Fn(BoundedError) -> Error,
{
    let mut collection =
        Collection::with_capacity(allocation_size_for_hint::<Collection::Item, MAX>(hint));

    // The input must supply the first `MIN` items.
    for index in 0..MIN {
        let Some(item) = next(&collection)? else {
            return Err(into_error(BoundedError::too_few(index, MIN)));
        };
        try_add::<Collection, MAX>(&mut collection, item, index).map_err(&into_error)?;
    }

    // The input may end at any point, as long as it stops by `MAX`.
    let mut index = MIN;
    while let Some(item) = next(&collection)? {
        try_add::<Collection, MAX>(&mut collection, item, index).map_err(&into_error)?;
        // `index` can go up to `MAX` which is of the same type, so no overflow risk
        // here.
        index += 1;
    }

    // At least `MIN` items were accepted, and none past `MAX`.
    Ok(Bounded::new_unchecked(collection))
}

/// Builds a bounded collection from an iterator.
///
/// The iterator's lower size bound only sizes the initial allocation.
pub fn collect_iter<Collection, Items, const MIN: usize, const MAX: usize>(
    items_iter: Items,
) -> Result<Bounded<Collection, MIN, MAX>, BoundedError>
where
    Collection: BoundedCollection,
    Items: IntoIterator<Item = Collection::Item>,
{
    let mut items = items_iter.into_iter();
    collect(Some(items.size_hint().0), |_| Ok(items.next()), identity)
}

/// Adds the item at input position `index`, refusing it past `MAX` or when it
/// is already held.
fn try_add<Collection, const MAX: usize>(
    collection: &mut Collection,
    item: Collection::Item,
    index: usize,
) -> Result<(), BoundedError>
where
    Collection: BoundedCollection,
{
    if index >= MAX {
        return Err(BoundedError::TooManyItems {
            count: index.saturating_add(1),
            max: MAX,
        });
    }
    if collection.add(item) {
        Ok(())
    } else {
        Err(BoundedError::DuplicateItem { index })
    }
}

/// Rejects a declared length outside `[MIN, MAX]` before any item is decoded.
///
/// Binary formats declare the length up front, so an input of the wrong size
/// fails here without a single item being decoded. Formats that declare
/// nothing, such as JSON, are held to the same bounds by [`collect`] as the
/// items arrive.
pub fn check_declared_len<Collection, Error, const MIN: usize, const MAX: usize>(
    hint: Option<usize>,
) -> Result<(), Error>
where
    Error: serde::de::Error,
{
    hint.map_or(Ok(()), |len| {
        Bounded::<Collection, MIN, MAX>::check_len_against_bounds(len).map_err(Error::custom)
    })
}

/// Deserializes a sequence into a bounded vector or ordered set.
pub struct SeqVisitor<Collection, const MIN: usize, const MAX: usize>(PhantomData<Collection>);

impl<Collection, const MIN: usize, const MAX: usize> SeqVisitor<Collection, MIN, MAX> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<'de, Collection, const MIN: usize, const MAX: usize> Visitor<'de>
    for SeqVisitor<Collection, MIN, MAX>
where
    Collection: BoundedCollection<Item: Deserialize<'de>>,
{
    type Value = Bounded<Collection, MIN, MAX>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a sequence of between {MIN} and {MAX} items")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let hint = sequence.size_hint();
        check_declared_len::<Collection, A::Error, MIN, MAX>(hint)?;
        collect(hint, |_| sequence.next_element(), A::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::collect_iter;
    use crate::bounded::{Bounded, BoundedError};

    type Collected<const MIN: usize, const MAX: usize> =
        Result<Bounded<Vec<u8>, MIN, MAX>, BoundedError>;

    #[test]
    fn an_input_that_ends_before_the_minimum_fails_where_it_ends() {
        let empty: Collected<2, 4> = collect_iter([]);
        let short: Collected<2, 4> = collect_iter([1]);

        assert_eq!(empty, Err(BoundedError::EmptyInput));
        assert_eq!(short, Err(BoundedError::TooFewItems { count: 1, min: 2 }));
    }

    #[test]
    fn items_past_the_minimum_are_optional() {
        let at_min: Collected<2, 4> = collect_iter([1, 2]);
        let at_max: Collected<2, 4> = collect_iter([1, 2, 3, 4]);

        assert_eq!(at_min.unwrap().into_inner(), [1, 2]);
        assert_eq!(at_max.unwrap().into_inner(), [1, 2, 3, 4]);
    }

    #[test]
    fn an_input_is_stopped_one_item_past_the_maximum() {
        let mut pulled = 0;

        let result: Collected<2, 4> = collect_iter((0..).inspect(|_| pulled += 1));

        assert_eq!(result, Err(BoundedError::TooManyItems { count: 5, max: 4 }));
        assert_eq!(pulled, 5);
    }
}
