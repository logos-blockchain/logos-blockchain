//! Canonical encoding shared by the keyed bounded collections: sets and maps.
//!
//! Both encode like a bounded vector, a `MAX`-width length prefix followed by
//! the items. What they add is a rule for the order of the items. A hash-backed
//! collection iterates in an order that changes between runs, so the items are
//! sorted by the bytes of each encoded key, in byte-wise lexicographic order.
//! That order needs no `Ord` on the key and can be stated in a specification
//! without reference to any language. Decoding requires strictly increasing
//! keys, so every value has exactly one encoding.
//!
//! A repeated key is rejected at the item that repeats it. That also bounds a
//! key type that decodes without consuming input: its second item is a
//! duplicate, so no length prefix can make the decode loop spin.

use core::{any::type_name, cmp::Ordering, ops::Range};

use super::{CodecExamples, CodecFixture, DecodeError};

/// Appends `items` to `out` in canonical order: ascending byte-wise order of
/// each item's encoded key.
///
/// `encode_item` appends one item's full encoding (key first) and returns the
/// length of its key. `items_encoded_length` is the total length of those
/// encodings, which sizes the scratch buffer they are sorted in.
pub(super) fn encode_items_sorted_by_key<Collection, Items, EncodeFn>(
    items: Items,
    items_encoded_length: usize,
    out: &mut Vec<u8>,
    mut encode_item: EncodeFn,
) where
    Items: ExactSizeIterator,
    EncodeFn: FnMut(Items::Item, &mut Vec<u8>) -> usize,
{
    struct EntryRange {
        key: Range<usize>,
        item: Range<usize>,
    }

    let mut encoded_items = Vec::with_capacity(items_encoded_length);
    let mut entry_ranges = Vec::with_capacity(items.len());
    for item in items {
        let start = encoded_items.len();
        let key_len = encode_item(item, &mut encoded_items);
        entry_ranges.push(EntryRange {
            key: start..start + key_len,
            item: start..encoded_items.len(),
        });
    }

    let key_bytes = |entry: &EntryRange| &encoded_items[entry.key.clone()];
    entry_ranges.sort_unstable_by(|a, b| key_bytes(a).cmp(key_bytes(b)));
    // Distinct keys must encode to distinct bytes, or the decoder rejects the
    // result as a duplicate. That is a contract violation of the key type's
    // codec, not something input can cause.
    debug_assert!(
        entry_ranges.is_sorted_by(|a, b| key_bytes(a) < key_bytes(b)),
        "{}: two distinct keys encode to the same bytes, so the encoding cannot be decoded; \
         the key type's encoding is not injective",
        type_name::<Collection>(),
    );
    for entry in entry_ranges {
        out.extend_from_slice(&encoded_items[entry.item]);
    }
}

/// Rejects the key at `index` unless it comes strictly after `previous` in
/// canonical order.
pub(super) fn check_canonical_order<T>(
    previous: Option<&[u8]>,
    key: &[u8],
    index: usize,
) -> Result<(), DecodeError>
where
    T: ?Sized,
{
    match previous.map(|previous| key.cmp(previous)) {
        // First item in the collection or any item greater than its predecessor.
        None | Some(Ordering::Greater) => Ok(()),
        Some(Ordering::Equal) => Err(DecodeError::duplicate_item::<T>(index)),
        Some(Ordering::Less) => Err(DecodeError::non_canonical_order::<T>(index)),
    }
}

/// The bytes an element decoder consumed: the part of `before` that is not
/// `after`, the remainder the decoder returned.
///
/// A decoder that honours the contract returns a suffix of its input, so
/// `after` is never longer than `before`. One that does not is reported as an
/// error naming the element type `T`, rather than a panic.
pub(super) fn consumed<'input, T>(
    before: &'input [u8],
    after: &[u8],
) -> Result<&'input [u8], DecodeError>
where
    T: ?Sized,
{
    let consumed_len = before.len().checked_sub(after.len()).ok_or_else(|| {
        DecodeError::custom(format!(
            "{} returned more input than it was given",
            type_name::<T>()
        ))
    })?;
    Ok(&before[..consumed_len])
}

/// Up to `MAX` fixtures of `T` with pairwise distinct bytes, in canonical
/// order: a collection's own fixture must list its keys in the order the
/// encoder emits them.
pub(super) fn distinct_fixtures_in_canonical_order<
    Collection,
    T,
    const MIN: usize,
    const MAX: usize,
>() -> Vec<CodecFixture<T>>
where
    T: CodecExamples,
{
    let mut fixtures: Vec<_> = T::fixtures().into_iter().collect();
    fixtures.sort_by(|a, b| a.bytes.cmp(&b.bytes));
    fixtures.dedup_by(|a, b| a.bytes == b.bytes);
    fixtures.truncate(MAX);
    require_enough_fixtures::<Collection, T, MIN>(fixtures.len());
    fixtures
}

/// A keyed collection's fixture needs `MIN` distinct keys, and the only source
/// of keys is the key type's own fixtures.
fn require_enough_fixtures<Collection, T, const MIN: usize>(available: usize) {
    assert!(
        available >= MIN,
        "{collection}: its fixture needs at least {MIN} distinct fixtures of {key}, but {key} \
         has {available}; add more to the `codec_fixtures!` of {key}",
        collection = type_name::<Collection>(),
        key = type_name::<T>(),
    );
}

/// The fixture of `V` at `index`, cycling through all of `V`'s fixtures.
///
/// Gives every entry of a map fixture a value without requiring `V: Clone`.
pub(super) fn cycled_fixture<V>(index: usize) -> CodecFixture<V>
where
    V: CodecExamples,
{
    let fixtures = V::fixtures();
    let position = index % fixtures.len();
    fixtures
        .into_iter()
        .nth(position)
        .expect("the position is within the fixtures")
}
