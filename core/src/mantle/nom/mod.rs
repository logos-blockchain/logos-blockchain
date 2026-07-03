use std::borrow::Cow;

// Re-exported so call sites can `use crate::mantle::nom::{NomCodec, nom_wire_fixtures}`.
// Both macros are the blessed way to declare a codec: each emits the mandatory
// `WireExamples` fixtures (see below) alongside the impls.
pub use lb_core_macros::{NomCodec, nom_wire_fixtures};
use lb_utils::bounded_vec::LowerBoundedVec;
use nom::IResult;

pub mod array;
pub mod bounded_vec;
pub mod core;
pub mod kms;
pub mod numbers;
pub mod proof_of_quota;
pub mod time;

mod fixtures;

// Both codec traits require `WireExamples` (see below): a type cannot be a wire
// codec without also pinning a well-known fixture. Because `WireExamples` is
// sealed, the only ways to satisfy it are `#[derive(NomCodec)]` and
// `nom_wire_fixtures!`, both of which demand a fixture — so `impl NomEncode for
// Foo` without a fixture is a `cargo build` error.
pub trait NomEncode: WireExamples {
    // TODO: This could be turned into a `BoundedVec<u8, MAX_BYTES>` if we are
    // always able to set an upper limit on everything that goes through NOM
    // decoding. That would allow us to set an upper bound on ANY nom-encoded
    // struct, including a mantle tx itself.
    fn encode(&self) -> Vec<u8>;
}

pub trait NomDecode: WireExamples {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self>;
}

// ==============================================================================
// Well-known fixtures
// ==============================================================================
// Every wire codec must ship at least one *well-known fixture*: a value
// together with its exact wire bytes. Fixtures pin the encoding against silent
// drift and feed the generated round-trip test (`assert_nom_wire_fixtures`).
//
// `WireExamples` is the prerequisite that makes a fixture impossible to forget.
// It is sealed (`sealed::Sealed`), so the only ways to satisfy it are
// `#[derive(NomCodec)]` and `nom_wire_fixtures!`, both of which demand a
// fixture. It is a supertrait of both codec traits (above), so `impl NomEncode
// for Foo` without a fixture is a `cargo build` error (E0277).

/// Carries the mandatory [`WireFixtures`] for a codec. The non-empty return
/// type means a codec cannot exist without at least one fixture.
pub trait WireExamples: sealed::Sealed + Sized {
    #[must_use]
    fn fixtures() -> WireFixtures<Self>;
}

pub(crate) mod sealed {
    /// Implementable only by the blessed macro path (`#[derive(NomCodec)]` /
    /// `nom_wire_fixtures!`). Being `pub(crate)` it is unnameable downstream,
    /// which seals [`super::WireExamples`] against external impls.
    pub trait Sealed {}
}

/// A single golden vector: a value and its exact wire bytes.
///
/// `bytes` is a [`Cow`] so leaf fixtures can borrow a `&'static` slice (emitted
/// by the macros) while generic blanket impls build theirs from the element's
/// fixtures ([`Cow::Owned`]).
pub struct WireFixture<T> {
    pub value: T,
    pub bytes: Cow<'static, [u8]>,
}

/// A codec's well-known fixtures: at least one `(value, bytes)` pair, up to as
/// many as needed. The `1`-lower-bounded type is what makes "a codec cannot
/// exist without a fixture" part of the contract.
pub type WireFixtures<T> = LowerBoundedVec<WireFixture<T>, 1>;

/// Drives every fixture of `T` through the wire-format invariants. Called by
/// the round-trip test the macros generate, and reusable for hand-written tests
/// of generic monomorphizations (e.g. `BoundedVec<u8, 2, 4>`).
#[cfg(test)]
pub(crate) fn assert_nom_wire_fixtures<T>()
where
    T: NomEncode + NomDecode + WireExamples + PartialEq + ::core::fmt::Debug,
{
    let type_name = ::core::any::type_name::<T>();

    for fixture in T::fixtures() {
        let expected = fixture.bytes.as_ref();

        // Golden encode: the value serializes to exactly the pinned bytes. On a
        // mismatch we print both sides hex-encoded — the `assert_eq!` default
        // would dump raw `[u8]` arrays in decimal.
        let encoded = fixture.value.encode();
        assert!(
            encoded.as_slice() == expected,
            "{type_name}: encode(value) drifted from the well-known bytes\n  value: {:?}  actual   (hex): {actual}\n  expected (hex): {expected_hex}",
            fixture.value,
            actual = hex::encode(&encoded),
            expected_hex = hex::encode(expected),
        );

        // Golden decode: the pinned bytes decode back to the value, leaving
        // nothing behind.
        let (rest, decoded) = T::decode(expected).unwrap_or_else(|err| {
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
            expected_value = &fixture.value,
        );

        // Round-trip: encode then decode is the identity (independent of the
        // pinned bytes, so it catches encode/decode asymmetry directly).
        let (rest, round_tripped) = T::decode(&encoded)
            .unwrap_or_else(|err| panic!("{type_name}: round-trip decode failed: {err:?}"));
        assert!(
            rest.is_empty(),
            "{type_name}: round-trip left trailing data (hex): {}",
            hex::encode(rest),
        );
        assert!(
            round_tripped == fixture.value,
            "{type_name}: round-trip changed the value\n  before: {before:?}\n  after:  {round_tripped:?}",
            before = &fixture.value,
        );
    }
}

// Simple utility to encode a slice of `NomEncode` items by encoding each item
// and concatenating the results. Not implemented on the slice type directly
// `[T]` since that could be misleading.
fn encode_slice<T: NomEncode>(items: &[T]) -> Vec<u8> {
    items.iter().flat_map(NomEncode::encode).collect()
}
