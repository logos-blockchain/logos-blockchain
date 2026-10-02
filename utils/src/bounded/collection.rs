//! Building a bounded collection item by item, from an iterator or from a
//! serde sequence or map.
//!
//! A bounded collection adds its bounds to the collection it wraps, and leaves
//! everything else to it:
//!
//! - No input may supply more than `MAX` items, repeats included, so building a
//!   collection never costs more than `MAX` items' worth of work. The item past
//!   `MAX` is refused where it arrives; a map refuses it on its key, before its
//!   value is decoded.
//! - Once the input ends, the collection must hold at least `MIN` items,
//!   counted after any repeats have merged.
//! - Deserialization treats a repeated element or key exactly as the wrapped
//!   collection's own `insert` does. Construction from an iterator is stricter,
//!   and rejects a repeat as an error.
//! - A declared length is trusted for pre-allocation only up to the budget
//!   every bounded collection shares (see [`allocation_size_for_hint`]), and
//!   one outside the bounds is rejected before a single item is decoded.

use core::{fmt, marker::PhantomData};

use serde::{
    Deserialize,
    de::{Error as _, MapAccess, SeqAccess, Visitor},
};

use crate::bounded::{Bounded, BoundedError, BoundedLen, allocation_size_for_hint};

/// A collection a bounded type can be built from, one item at a time.
pub trait BoundedCollection: BoundedLen + Sized {
    /// What one insertion adds.
    type Item;

    fn with_capacity(capacity: usize) -> Self;

    /// Adds `item` as the collection's own `insert` does, and returns whether
    /// the collection grew: a vector appends every item, a set keeps the
    /// element it already holds, and a map replaces the value under a key it
    /// already holds.
    fn add(&mut self, item: Self::Item) -> bool;
}

/// Builds a bounded collection from an iterator, rejecting a repeated item.
///
/// Iteration stops at the first repeat or at the first item past `MAX`. The
/// iterator's lower size bound only sizes the initial allocation.
pub fn collect_iter<Collection, Items, const MIN: usize, const MAX: usize>(
    items: Items,
) -> Result<Bounded<Collection, MIN, MAX>, BoundedError>
where
    Collection: BoundedCollection,
    Items: IntoIterator<Item = Collection::Item>,
{
    let items = items.into_iter();
    let mut collection = Collection::with_capacity(
        allocation_size_for_hint::<Collection::Item, MAX>(Some(items.size_hint().0)),
    );
    for (index, item) in items.enumerate() {
        check_position::<MAX>(index)?;
        // A repeat has already been merged by the time it is found, but the
        // collection is dropped with the error.
        if !collection.add(item) {
            return Err(BoundedError::DuplicateItem { index });
        }
    }
    Bounded::try_new(collection)
}

/// Refuses the item at input position `index` when it is past `MAX`.
const fn check_position<const MAX: usize>(index: usize) -> Result<(), BoundedError> {
    if index >= MAX {
        return Err(BoundedError::TooManyItems {
            count: index.saturating_add(1),
            max: MAX,
        });
    }
    Ok(())
}

/// Rejects a declared length outside `[MIN, MAX]` before any item is decoded.
///
/// Binary formats declare the length up front, so an input of the wrong size
/// fails here without a single item being decoded: one declaring more than
/// `MAX` items would supply them, and one declaring fewer than `MIN` could
/// never leave the collection holding `MIN`. Formats that declare nothing,
/// such as JSON, are held to the same bounds as the items arrive.
fn check_declared_len<Collection, Error, const MIN: usize, const MAX: usize>(
    hint: Option<usize>,
) -> Result<(), Error>
where
    Error: serde::de::Error,
{
    hint.map_or(Ok(()), |len| {
        Bounded::<Collection, MIN, MAX>::check_len_against_bounds(len).map_err(Error::custom)
    })
}

/// Deserializes a sequence into a bounded collection, one element at a time.
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
        write!(
            formatter,
            "a sequence of at most {MAX} items, making a collection of at least {MIN}"
        )
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let hint = sequence.size_hint();
        check_declared_len::<Collection, A::Error, MIN, MAX>(hint)?;
        let mut collection =
            Collection::with_capacity(allocation_size_for_hint::<Collection::Item, MAX>(hint));
        let mut index = 0;
        while let Some(item) = sequence.next_element()? {
            check_position::<MAX>(index).map_err(A::Error::custom)?;
            collection.add(item);
            // `index` stays below `MAX`, so it cannot overflow.
            index += 1;
        }
        Bounded::try_new(collection).map_err(A::Error::custom)
    }
}

/// Deserializes a map into a bounded map, one entry at a time, reading each
/// entry key first.
///
/// The entry past `MAX` is refused on its key, before its value is decoded.
pub struct MapVisitor<Map, const MIN: usize, const MAX: usize>(PhantomData<Map>);

impl<Map, const MIN: usize, const MAX: usize> MapVisitor<Map, MIN, MAX> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<'de, Map, K, V, const MIN: usize, const MAX: usize> Visitor<'de> for MapVisitor<Map, MIN, MAX>
where
    Map: BoundedCollection<Item = (K, V)>,
    K: Deserialize<'de>,
    V: Deserialize<'de>,
{
    type Value = Bounded<Map, MIN, MAX>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a map of at most {MAX} entries, making a map of at least {MIN}"
        )
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let hint = entries.size_hint();
        check_declared_len::<Map, A::Error, MIN, MAX>(hint)?;
        let mut map = Map::with_capacity(allocation_size_for_hint::<(K, V), MAX>(hint));
        let mut index = 0;
        while let Some(key) = entries.next_key()? {
            check_position::<MAX>(index).map_err(A::Error::custom)?;
            let value = entries.next_value()?;
            map.add((key, value));
            // `index` stays below `MAX`, so it cannot overflow.
            index += 1;
        }
        Bounded::try_new(map).map_err(A::Error::custom)
    }
}

#[cfg(test)]
pub mod test_utils {
    use core::fmt::Debug;

    use serde::{Serialize, de::DeserializeOwned};

    /// Asserts that a bounded collection reads `json` and `bincode` exactly as
    /// the collection it wraps does, and writes the result back alike.
    pub fn assert_serde_matches_underlying<Wrapper, Inner>(json: &str, bincode: &[u8])
    where
        Wrapper: Serialize + DeserializeOwned + AsRef<Inner> + Debug,
        Inner: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        let inner: Inner = serde_json::from_str(json).unwrap();
        let wrapper: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(wrapper.as_ref(), &inner, "reading JSON {json}");
        assert_eq!(
            serde_json::to_string(&wrapper).unwrap(),
            serde_json::to_string(&inner).unwrap(),
            "writing back JSON {json}"
        );

        let inner: Inner = bincode::deserialize(bincode).unwrap();
        let wrapper: Wrapper = bincode::deserialize(bincode).unwrap();
        assert_eq!(wrapper.as_ref(), &inner, "reading bincode {bincode:?}");
        assert_eq!(
            bincode::serialize(&wrapper).unwrap(),
            bincode::serialize(&inner).unwrap(),
            "writing back bincode {bincode:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::collect_iter;
    use crate::bounded::{Bounded, BoundedError};

    type Collected<const MIN: usize, const MAX: usize> =
        Result<Bounded<Vec<u8>, MIN, MAX>, BoundedError>;

    #[test]
    fn an_input_that_ends_before_the_minimum_fails() {
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
