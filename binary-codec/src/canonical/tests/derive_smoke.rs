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

/// The marked field decodes with the struct's context, the others with `()`.
#[derive(Debug, PartialEq, Eq, BinaryEncode, BinaryDecode)]
struct WithContext {
    a: u8,
    #[context]
    capped: Capped,
    b: u16,
}

codec_fixtures!(
    WithContext,
    context = 0x10,
    Self { a: 0x07, capped: Capped(0x0A), b: 0x0201 } => "070a0102"
);

/// A marked field can be derived itself, so the context climbs through it.
#[derive(Debug, PartialEq, Eq, BinaryEncode, BinaryDecode)]
struct Nested(u8, #[context] WithContext);

codec_fixtures!(
    Nested,
    context = 0x10,
    Self(0xFF, WithContext { a: 0x07, capped: Capped(0x0A), b: 0x0201 }) => "ff070a0102"
);

#[test]
fn the_context_reaches_the_marked_field() {
    let bytes = [0xFF, 0x07, 0x0A, 0x01, 0x02];

    assert!(Nested::decode(&bytes, &0x0A).is_ok());
    assert!(Nested::decode(&bytes, &0x09).is_err());
}

/// Decoded as its raw `u8`, which its `TryFrom` then checks.
#[derive(Debug, PartialEq, Eq, BinaryEncode, BinaryDecode)]
#[decode(try_from = "u8")]
struct Even(u8);

impl TryFrom<u8> for Even {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value.is_multiple_of(2) {
            Ok(Self(value))
        } else {
            Err(())
        }
    }
}

codec_fixtures!(Even, Self(0x02) => "02");

#[test]
fn a_conversion_that_fails_is_an_invalid_value() {
    assert!(matches!(
        Even::decode(&[0x03], &()),
        Err(DecodeError::InvalidValue { .. })
    ));
}

/// A conversion can apply to an enum, whose variants the derive never sees.
#[derive(Debug, PartialEq, Eq, BinaryDecode)]
#[decode(try_from = "u8")]
enum Discriminant {
    One,
}

impl TryFrom<u8> for Discriminant {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        (value == 1).then_some(Self::One).ok_or(())
    }
}

codec_fixtures!(Discriminant, decode_only, Self::One => "01");

/// Takes the context of the type it converts from.
#[derive(Debug, PartialEq, Eq, BinaryDecode)]
#[decode(from = "Capped")]
struct FromCapped(u8);

impl From<Capped> for FromCapped {
    fn from(Capped(value): Capped) -> Self {
        Self(value)
    }
}

codec_fixtures!(FromCapped, decode_only, context = 0x10, Self(0x0A) => "0a");

#[test]
fn a_conversion_passes_its_context_down() {
    assert!(FromCapped::decode(&[0x0A], &0x0A).is_ok());
    assert!(FromCapped::decode(&[0x0A], &0x09).is_err());
}

/// Deriving only the decoder needs no encoder at all.
#[derive(Debug, PartialEq, Eq, BinaryDecode)]
struct DecodeOnly(u8, u16);

codec_fixtures!(DecodeOnly, decode_only, Self(0x07, 0x0201) => "070102");
