//! The unified wire codec for Logos blockchain components.
//!
//! One encode trait ([`WireEncode`]) and one decode trait ([`WireDecode`]) that
//! every on-the-wire type implements, replacing the previously separate
//! `WireEncode`/`WireDecode` (Mantle) and `WireEncode`/`WireDecode` (Blend)
//! families. Primitives, fixed-size arrays and `BoundedVec` get default impls
//! here.
//!
//! Every codec must also ship at least one **well-known fixture** (a value and
//! its exact wire bytes). This is enforced at compile time: both codec traits
//! require [`WireExamples`], whose only sanctioned implementation path is
//! [`wire_fixtures!`] / `#[derive(WireCodec)]`, so a codec without a fixture is
//! a compilation error.

// The derive and `wire_fixtures!` expansions refer to this crate as
// `::lb_wire`, so the crate must be able to name itself that way when it uses
// them for its own primitives.
extern crate self as lb_wire;

mod array;
mod boolean;
mod bounded_vec;
mod error;
mod fixtures;
mod numbers;

#[cfg(test)]
mod tests;

pub use error::DecodeError;
pub use fixtures::{
    WireExamples, WireFixture, WireFixtures, assert_wire_fixtures,
    assert_wire_fixtures_decode_only, assert_wire_fixtures_decode_only_with,
    assert_wire_fixtures_encode_only, assert_wire_fixtures_with, decode_fixture_hex,
};
pub use lb_wire_macros::{WireCodec, wire_fixtures};

/// Sealed marker that gates [`WireExamples`] to the blessed macro path.
///
/// `#[doc(hidden)] pub` (rather than `pub(crate)`) so the `wire_fixtures!` /
/// `#[derive(WireCodec)]` expansions can implement it from any downstream
/// crate; undocumented, so the macros remain the only sanctioned way to satisfy
/// it.
#[doc(hidden)]
pub mod sealed {
    pub trait Sealed {}
}

/// Append a value's wire bytes to a caller-owned buffer.
///
/// Requires [`WireExamples`]: a type cannot be a wire codec without also
/// pinning a well-known fixture.
pub trait WireEncode: WireExamples {
    /// The exact number of bytes [`encode_into`](Self::encode_into) will
    /// append, computed without encoding or allocating.
    fn encoded_length(&self) -> usize;

    /// Append this value's wire bytes to `out`. The single required
    /// serialization primitive; composites chain their children's
    /// `encode_into`.
    fn encode_into(&self, out: &mut Vec<u8>);

    /// Encode into a freshly allocated, exactly-sized boxed slice.
    fn encode(&self) -> Box<[u8]> {
        self.encode_to_vec().into_boxed_slice()
    }

    /// Encode into a freshly allocated `Vec<u8>` — the ergonomic bridge for the
    /// many call sites that feed encoded bytes into a `Vec<u8>` sink.
    fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_length());
        self.encode_into(&mut out);
        out
    }
}

/// Decode a value from the front of `input`, returning it and the unconsumed
/// remainder (`(rest, value)`, as in `nom`).
///
/// `Context` carries anything the decoder needs that is not on the wire (e.g. a
/// layer count); it is `()` for self-describing components. Requires
/// [`WireExamples`] for the same reason as [`WireEncode`].
pub trait WireDecode: WireExamples + Sized {
    type Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError>;
}

/// Ergonomic decode for the common `Context = ()` case:
/// `T::decode(bytes)` instead of `T::decode(bytes, ())`.
pub trait WireDecodeExt: WireDecode<Context = ()> {
    fn decode(input: &[u8]) -> Result<(&[u8], Self), DecodeError> {
        <Self as WireDecode>::decode(input, &())
    }
}

impl<T> WireDecodeExt for T where T: WireDecode<Context = ()> {}

/// Split `n` bytes off the front of `input`, returning `(head, rest)`, or fail
/// with [`DecodeError::UnexpectedEnd`] naming `T`.
///
/// The checked building block for fixed-size decoders — use it instead of
/// `split_at`/indexing so malformed (short) input is a typed error, not a
/// panic.
pub fn take<T>(input: &[u8], n: usize) -> Result<(&[u8], &[u8]), DecodeError>
where
    T: ?Sized,
{
    input
        .split_at_checked(n)
        .ok_or_else(|| DecodeError::end_of_input::<T>(n.saturating_sub(input.len())))
}
