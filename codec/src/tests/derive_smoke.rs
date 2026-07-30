//! Smoke test for `#[derive(BinaryCodec)]`, kept in-crate so the
//! `::lb_codec::` paths the derive emits resolve via `extern crate self as
//! lb_codec`.

use crate::{BinaryCodec, BinaryDecodeExt as _, BinaryEncode as _, codec_fixtures};

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

    let (rest, decoded) = Named::decode(&bytes).unwrap();
    assert!(rest.is_empty());
    assert_eq!(decoded, value);
}

#[test]
fn derived_tuple_struct_round_trips() {
    let value = Tuple(0x11, 0x2233_4455);
    let bytes = value.encode_to_vec();
    assert_eq!(value.encoded_length(), bytes.len());

    let (rest, decoded) = Tuple::decode(&bytes).unwrap();
    assert!(rest.is_empty());
    assert_eq!(decoded, value);
}
