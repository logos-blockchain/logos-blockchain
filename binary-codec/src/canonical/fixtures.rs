use std::borrow::Cow;

use lb_utils::bounded::LowerBoundedVec;

use super::{BinaryDecode, BinaryEncode};

/// Carries the mandatory [`CodecFixtures`] for a codec. The non-empty return
/// type means a codec cannot exist without at least one fixture.
///
/// Sealed via [`super::sealed::Sealed`], so the only ways to satisfy it are
/// `#[derive(BinaryCodec)]` and `codec_fixtures!`, both of which demand a
/// fixture. It is a supertrait of both codec traits, so `impl BinaryEncode for
/// Foo` without a fixture is a compilation error.
pub trait CodecExamples: super::sealed::Sealed + Sized {
    #[must_use]
    fn fixtures() -> CodecFixtures<Self>;
}

/// A single golden vector: a value and its exact encoded bytes.
///
/// `bytes` is a [`Cow`] so leaf fixtures can borrow a `&'static` slice (emitted
/// by the macros) while generic blanket impls build theirs from the element's
/// fixtures ([`Cow::Owned`]).
pub struct CodecFixture<T> {
    pub value: T,
    pub bytes: Cow<'static, [u8]>,
}

/// A codec's well-known fixtures: at least one `(value, bytes)` pair, up to as
/// many as needed. The `1`-lower-bounded type is what makes "a codec cannot
/// exist without a fixture" part of the contract.
pub type CodecFixtures<T> = LowerBoundedVec<CodecFixture<T>, 1>;

/// Decode a well-known fixture's hex string to bytes, ignoring ASCII whitespace
/// (so an `include_str!`-ed `.hex` file may contain newlines). Panics on
/// invalid hex — fixtures are authored test vectors, so bad hex is a bug, not a
/// runtime condition. Emitted by `codec_fixtures!` for its non-literal
/// (`include_str!`) byte form.
#[doc(hidden)]
#[must_use]
pub fn decode_fixture_hex(hex_str: &str) -> Vec<u8> {
    let compact: String = hex_str.split_whitespace().collect();
    hex::decode(compact).expect("well-known fixture is valid hex")
}

/// Drives every fixture of a `Context = ()` codec through the encoding
/// invariants. Called by the round-trip test the macros generate.
///
/// `#[doc(hidden)] pub` (not `#[cfg(test)]`) because the generated test lives
/// in *downstream* crates and calls this against `lb-binary-codec`'s non-test
/// build.
#[doc(hidden)]
pub fn assert_codec_fixtures<T>()
where
    T: BinaryEncode + BinaryDecode<Context = ()> + PartialEq + core::fmt::Debug,
{
    assert_codec_fixtures_with::<T, _>(|| ());
}

/// Like [`assert_codec_fixtures`], but for encode-only codecs (types that
/// implement [`BinaryEncode`] but not [`BinaryDecode`], e.g. post-verification
/// wrappers). Checks the golden bytes and `encoded_length`; there is no decode
/// or round-trip leg.
#[doc(hidden)]
pub fn assert_codec_fixtures_encode_only<T>()
where
    T: BinaryEncode + core::fmt::Debug,
{
    let type_name = core::any::type_name::<T>();

    for fixture in T::fixtures() {
        let expected = fixture.bytes.as_ref();
        let encoded = fixture.value.encode();
        assert!(
            &*encoded == expected,
            "{type_name}: encode(value) drifted from the well-known bytes\n  value: {:?}\n  actual   (hex): {actual}\n  expected (hex): {expected_hex}",
            fixture.value,
            actual = hex::encode(&*encoded),
            expected_hex = hex::encode(expected),
        );
        assert_eq!(
            fixture.value.encoded_length(),
            encoded.len(),
            "{type_name}: encoded_length() disagrees with encode().len()",
        );
    }
}

/// Like [`assert_codec_fixtures`], but for decode-only codecs (types that
/// implement [`BinaryDecode`] but not [`BinaryEncode`], e.g. messages that are
/// only ever received from a peer). Decodes the well-known bytes and checks the
/// value equals the fixture's reference value, with nothing left over.
#[doc(hidden)]
pub fn assert_codec_fixtures_decode_only<T>()
where
    T: BinaryDecode<Context = ()> + PartialEq + core::fmt::Debug,
{
    assert_codec_fixtures_decode_only_with::<T, _>(|| ());
}

/// Like [`assert_codec_fixtures_decode_only`], but for decode-only codecs
/// whose `Context` is not `()`.
#[doc(hidden)]
pub fn assert_codec_fixtures_decode_only_with<T, ContextBuilder>(make_context: ContextBuilder)
where
    T: BinaryDecode + PartialEq + core::fmt::Debug,
    ContextBuilder: Fn() -> T::Context,
{
    let type_name = core::any::type_name::<T>();

    for fixture in T::fixtures() {
        let expected = fixture.bytes.as_ref();
        let context = make_context();
        let (rest, decoded) = T::decode(expected, &context).unwrap_or_else(|err| {
            panic!(
                "{type_name}: well-known bytes failed to decode: {err:?}\n  bytes (hex): {}",
                hex::encode(expected),
            )
        });
        assert!(
            rest.is_empty(),
            "{type_name}: well-known bytes left trailing data (hex): {}",
            hex::encode(rest),
        );
        assert!(
            decoded == fixture.value,
            "{type_name}: decode(bytes) != reference value\n  bytes (hex): {bytes}\n  decoded:  {decoded:?}\n  expected: {expected_value:?}",
            bytes = hex::encode(expected),
            expected_value = fixture.value,
        );
    }
}

/// Like [`assert_codec_fixtures`], but for codecs whose `Context` is not
/// `()`: `make_context` produces a fresh context per decode.
#[doc(hidden)]
pub fn assert_codec_fixtures_with<T, ContextBuilder>(make_context: ContextBuilder)
where
    T: BinaryEncode + BinaryDecode + PartialEq + core::fmt::Debug,
    ContextBuilder: Fn() -> T::Context,
{
    let type_name = core::any::type_name::<T>();

    for fixture in T::fixtures() {
        let expected = fixture.bytes.as_ref();

        // Golden encode: the value serializes to exactly the pinned bytes. On a
        // mismatch we print both sides hex-encoded — the `assert_eq!` default
        // would dump raw `[u8]` arrays in decimal.
        let encoded = fixture.value.encode();
        assert!(
            &*encoded == expected,
            "{type_name}: encode(value) drifted from the well-known bytes\n  value: {:?}\n  actual   (hex): {actual}\n  expected (hex): {expected_hex}",
            fixture.value,
            actual = hex::encode(&*encoded),
            expected_hex = hex::encode(expected),
        );

        // `encoded_length` must agree with the real byte count, or release
        // builds (where downstream `debug_assert`s are gone) silently mis-size.
        assert_eq!(
            fixture.value.encoded_length(),
            encoded.len(),
            "{type_name}: encoded_length() disagrees with encode().len()",
        );

        // Golden decode: the pinned bytes decode back to the value, leaving
        // nothing behind.
        let context = make_context();
        let (rest, decoded) = T::decode(expected, &context).unwrap_or_else(|err| {
            panic!(
                "{type_name}: well-known bytes failed to decode: {err:?}\n  bytes (hex): {}",
                hex::encode(expected),
            )
        });
        assert!(
            rest.is_empty(),
            "{type_name}: well-known bytes left trailing data (hex): {}",
            hex::encode(rest),
        );
        assert!(
            decoded == fixture.value,
            "{type_name}: decode(bytes) != value\n  bytes (hex): {bytes}\n  decoded:  {decoded:?}\n  expected: {expected_value:?}",
            bytes = hex::encode(expected),
            expected_value = fixture.value,
        );

        // Re-encode: the decoded value encodes back to exactly the pinned
        // bytes. `==` can be coarser than the encoding (a type may leave a
        // field out of its equality that its encoding keeps), so the check
        // above cannot see a decoder that changes what `==` ignores. This one
        // can.
        let reencoded = decoded.encode();
        assert!(
            &*reencoded == expected,
            "{type_name}: encode(decode(bytes)) differs from the well-known bytes\n  actual   (hex): {actual}\n  expected (hex): {expected_hex}",
            actual = hex::encode(&*reencoded),
            expected_hex = hex::encode(expected),
        );

        // Round-trip: encode then decode is the identity (independent of the
        // pinned bytes, so it catches encode/decode asymmetry directly).
        let (rest, round_tripped) = T::decode(&encoded, &context)
            .unwrap_or_else(|err| panic!("{type_name}: round-trip decode failed: {err:?}"));
        assert!(
            rest.is_empty(),
            "{type_name}: round-trip left trailing data (hex): {}",
            hex::encode(rest),
        );
        assert!(
            round_tripped == fixture.value,
            "{type_name}: round-trip changed the value\n  before: {before:?}\n  after:  {round_tripped:?}",
            before = fixture.value,
        );
    }
}

/// Up to `MAX` fixtures of `T` with pairwise distinct bytes, in the order `T`
/// declares them: what an ordered collection's own fixture is built from.
///
/// The collection's fixture needs `MIN` distinct items, and the only source of
/// items is the item type's own fixtures, so fewer than `MIN` of them is a
/// panic that says what to add.
pub(super) fn distinct_fixtures_in_declared_order<
    Collection,
    T,
    const MIN: usize,
    const MAX: usize,
>() -> Vec<CodecFixture<T>>
where
    T: CodecExamples,
{
    let mut fixtures: Vec<CodecFixture<T>> = Vec::new();
    for fixture in T::fixtures() {
        if fixtures.len() == MAX {
            break;
        }
        if fixtures.iter().all(|kept| kept.bytes != fixture.bytes) {
            fixtures.push(fixture);
        }
    }
    assert!(
        fixtures.len() >= MIN,
        "{collection}: its fixture needs at least {MIN} distinct fixtures of {item}, but {item} \
         has {available}; add more to the `codec_fixtures!` of {item}",
        collection = core::any::type_name::<Collection>(),
        item = core::any::type_name::<T>(),
        available = fixtures.len(),
    );
    fixtures
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
