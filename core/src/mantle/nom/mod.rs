use std::borrow::Cow;

// Re-exported so call sites can `use crate::mantle::nom::{NomCodec, wire_fixture}`.
// Both macros are the blessed way to declare a codec: each emits the mandatory
// `WireExamples` fixture (see below) alongside the impls.
pub use lb_core_macros::{NomCodec, wire_fixture};
use nom::IResult;

pub mod array;
pub mod bounded_vec;
pub mod core;
pub mod kms;
pub mod numbers;
pub mod proof_of_quota;

// Both codec traits require `WireExamples` (see below): a type cannot be a wire
// codec without also pinning a well-known fixture. Because `WireExamples` is
// sealed, the only ways to satisfy it are `#[derive(NomCodec)]` and
// `wire_fixture!`, both of which demand a fixture — so `impl NomEncode for Foo`
// without a fixture is a `cargo build` error.
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
// Every wire codec must ship a *well-known fixture*: a canonical value together
// with its exact wire bytes. The fixture pins the encoding against silent drift
// and feeds the generated round-trip test (`assert_wire_fixtures`).
//
// `WireExamples` is the prerequisite that makes a fixture impossible to forget.
// It is sealed (`sealed::Sealed`), so the only ways to satisfy it are
// `#[derive(NomCodec)]` and `wire_fixture!`, both of which demand a fixture. It
// is a supertrait of both codec traits (above), so `impl NomEncode for Foo`
// without a fixture is a `cargo build` error (E0277).
//
// The one gap the type system cannot close: a NEW monomorphization of an
// already-sealed generic (e.g. `BoundedVec<NewType, 0, 99>`) gets fixture
// existence from the blanket impl but no hand-pinned drift fixture. A CI lint
// counting `impl NomEncode` sites is the intended backstop there.

/// A single golden vector: a canonical value and its exact wire bytes.
///
/// `bytes` is a [`Cow`] so leaf fixtures can borrow a `&'static` slice (emitted
/// by the macros) while generic blanket impls build theirs from the element's
/// fixture ([`Cow::Owned`]).
pub struct WireFixture<T> {
    pub value: T,
    pub bytes: Cow<'static, [u8]>,
}

pub(crate) mod sealed {
    /// Implementable only by the blessed macro path (`#[derive(NomCodec)]` /
    /// `wire_fixture!`). Being `pub(crate)` it is unnameable downstream, which
    /// seals [`super::WireExamples`] against external impls.
    pub trait Sealed {}
}

/// Carries the mandatory [`WireFixture`]s for a codec. No default body for
/// [`Self::canonical_fixture`] means a codec cannot exist without one.
pub trait WireExamples: sealed::Sealed + Sized {
    /// The canonical fixture every codec must provide.
    #[must_use]
    fn canonical_fixture() -> WireFixture<Self>;

    /// Additional fixtures (edge cases, alternate encodings). Optional.
    #[must_use]
    fn extra_fixtures() -> Vec<WireFixture<Self>> {
        Vec::new()
    }

    /// The canonical fixture followed by any extras.
    #[must_use]
    fn fixtures() -> Vec<WireFixture<Self>> {
        let mut all = vec![Self::canonical_fixture()];
        all.extend(Self::extra_fixtures());
        all
    }
}

/// Drives every fixture of `T` through the wire-format invariants. Called by
/// the round-trip test the macros generate, and reusable for hand-written tests
/// of generic monomorphizations (e.g. `BoundedVec<u8, 2, 4>`).
#[cfg(test)]
pub(crate) fn assert_wire_fixtures<T>()
where
    T: NomEncode + NomDecode + WireExamples + PartialEq + ::core::fmt::Debug,
{
    for fixture in T::fixtures() {
        // Golden encode: the value serializes to exactly the pinned bytes.
        let encoded = fixture.value.encode();
        assert_eq!(
            encoded.as_slice(),
            fixture.bytes.as_ref(),
            "encode(value) drifted from the well-known bytes",
        );

        // Golden decode: the pinned bytes decode back to the value, leaving
        // nothing behind.
        let (rest, decoded) =
            T::decode(fixture.bytes.as_ref()).expect("well-known bytes failed to decode");
        assert!(rest.is_empty(), "well-known bytes left trailing data");
        assert_eq!(decoded, fixture.value, "decode(bytes) != value");

        // Round-trip: encode then decode is the identity (independent of the
        // pinned bytes, so it catches encode/decode asymmetry directly).
        let (rest, round_tripped) = T::decode(&encoded).expect("round-trip decode failed");
        assert!(rest.is_empty(), "round-trip left trailing data");
        assert_eq!(round_tripped, fixture.value, "round-trip changed the value");
    }
}

// Simple utility to encode a slice of `NomEncode` items by encoding each item
// and concatenating the results. Not implemented on the slice type directly
// `[T]` since that could be misleading.
fn encode_slice<T: NomEncode>(items: &[T]) -> Vec<u8> {
    items.iter().flat_map(NomEncode::encode).collect()
}
