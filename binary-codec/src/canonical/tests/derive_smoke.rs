//! Smoke test for `#[derive(BinaryCodec)]`, kept in-crate so the
//! `::lb_binary_codec::canonical::` paths emitted by the derive resolve via
//! the umbrella serialization crate.

use lb_utils::bounded::{BoundedIndexMap, BoundedSet};

use crate::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, codec_fixtures};

#[derive(Debug, PartialEq, Eq, BinaryCodec)]
struct Named {
    a: u8,
    b: u16,
}

codec_fixtures!(Named, Self { a: 0x07, b: 0x0201 } => "070102");

#[derive(Debug, PartialEq, Eq, BinaryCodec)]
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
    assert!(rest.is_empty());
    assert_eq!(decoded, value);
}

#[test]
fn derived_tuple_struct_round_trips() {
    let value = Tuple(0x11, 0x2233_4455);
    let bytes = value.encode_to_vec();
    assert_eq!(value.encoded_length(), bytes.len());

    let (rest, decoded) = Tuple::decode(&bytes, &()).unwrap();
    assert!(rest.is_empty());
    assert_eq!(decoded, value);
}

/// Keyed collection fields derive like any other field: each encodes in
/// declaration order under its own rules. The set sorts `{7, 0}` to `00 07`;
/// the index map keeps `9` before `3`.
#[derive(Debug, PartialEq, Eq)]
struct WithKeyed {
    tags: BoundedSet<u8, 0, 4>,
    slots: BoundedIndexMap<u8, u16, 1, 4>,
}

impl BinaryEncode for WithKeyed {
    fn encoded_length(&self) -> usize {
        self.tags.encoded_length() + self.slots.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.tags.encode_into(out);
        self.slots.encode_into(out);
    }
}

impl BinaryDecode for WithKeyed {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), crate::canonical::DecodeError> {
        let (rest, tags) = <BoundedSet<u8, 0, 4> as BinaryDecode>::decode(input, context)?;
        let (rest, slots) =
            <BoundedIndexMap<u8, u16, 1, 4> as BinaryDecode>::decode(rest, &((), ()))?;

        Ok((rest, Self { tags, slots }))
    }
}

codec_fixtures!(
    WithKeyed,
    Self {
        tags: BoundedSet::try_from_iter([7, 0]).unwrap(),
        slots: BoundedIndexMap::try_from_iter([(9, 0x0201), (3, 0x0403)]).unwrap(),
    } => "02000702090102030304"
);
