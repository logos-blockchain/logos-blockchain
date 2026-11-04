//! Smoke test for `#[derive(BinaryEncode)]` and `#[derive(BinaryDecode)]`, kept
//! in-crate so the `::lb_binary_codec::canonical::` paths emitted by the
//! derives resolve via the umbrella serialization crate.

use lb_utils::bounded::BoundedOrderedSet;

use crate::canonical::{BinaryDecode, BinaryEncode, DecodeError, codec_fixtures};

#[derive(Debug, PartialEq, Eq, BinaryEncode, BinaryDecode)]
struct Named {
    a: u8,
    b: u16,
}

codec_fixtures!(Named, Self { a: 0x07, b: 0x0201 } => "070102");

#[derive(Debug, PartialEq, Eq, BinaryEncode, BinaryDecode)]
struct Tuple(u8, u32);

codec_fixtures!(Tuple, Self(0xAB, 0x0403_0201) => "ab01020304");

#[test]
fn derived_named_struct_round_trips() {
    let value = Named { a: 9, b: 0xBEEF };
    let bytes = value.encode_to_vec();
    // fields in declaration order: `a` (1 byte) then little-endian `b` (2 bytes).
    assert_eq!(bytes, vec![9, 0xEF, 0xBE]);
    assert_eq!(value.encoded_length(), 3);

    let (rest, decoded) = Named::decode(&bytes, &()).unwrap();
    assert_eq!(rest, b"");
    assert_eq!(decoded, value);
}

#[test]
fn derived_tuple_struct_round_trips() {
    let value = Tuple(0x11, 0x2233_4455);
    let bytes = value.encode_to_vec();
    assert_eq!(value.encoded_length(), bytes.len());

    let (rest, decoded) = Tuple::decode(&bytes, &()).unwrap();
    assert_eq!(rest, b"");
    assert_eq!(decoded, value);
}

/// An ordered set field derives like any other field: it encodes in its own
/// order, `7` before `0`, between its neighbours.
#[derive(Debug, PartialEq, Eq, BinaryEncode, BinaryDecode)]
struct WithOrderedSet {
    tags: BoundedOrderedSet<u8, 0, 4>,
    count: u16,
}

codec_fixtures!(
    WithOrderedSet,
    Self {
        tags: BoundedOrderedSet::try_from_iter([7, 0]).unwrap(),
        count: 0x0201,
    } => "0207000102"
);

/// Deriving only the encoder leaves the decoder to the type, which here needs a
/// context the derive does not support.
#[derive(Debug, PartialEq, Eq, BinaryEncode)]
struct Capped(u8);

impl BinaryDecode for Capped {
    /// The largest value decoding accepts.
    type Context = u8;

    fn decode<'input>(
        input: &'input [u8],
        max: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, value) = u8::decode(input, &())?;
        if value > *max {
            return Err(DecodeError::invalid_value::<Self>("above the maximum"));
        }
        Ok((rest, Self(value)))
    }
}

codec_fixtures!(Capped, context = 0x10, Self(0x0A) => "0a");

/// Deriving only the decoder needs no encoder at all.
#[derive(Debug, PartialEq, Eq, BinaryDecode)]
struct DecodeOnly(u8, u16);

codec_fixtures!(DecodeOnly, decode_only, Self(0x07, 0x0201) => "070102");
