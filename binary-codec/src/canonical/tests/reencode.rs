//! The fixture harness must catch a decoder that loses what `==` ignores.

use std::borrow::Cow;

use lb_utils::bounded::BoundedIndexMap;

use crate::canonical::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    assert_codec_fixtures, sealed,
};

type Entries = BoundedIndexMap<u8, u8, 0, 4>;

/// Encodes like [`Entries`] but decodes the entries in reverse. `==` on an
/// index map ignores order, so the decoded value still compares equal.
#[derive(Debug, PartialEq, Eq)]
struct ReversingDecoder(Entries);

impl sealed::Sealed for ReversingDecoder {}

// Written by hand rather than with `codec_fixtures!`, whose generated test
// would fail on this deliberately broken codec.
impl CodecExamples for ReversingDecoder {
    fn fixtures() -> CodecFixtures<Self> {
        [CodecFixture {
            value: Self(Entries::try_from_iter([(1, 10), (2, 20)]).unwrap()),
            bytes: Cow::Borrowed(&[2, 1, 10, 2, 20]),
        }]
        .into()
    }
}

impl BinaryEncode for ReversingDecoder {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

impl BinaryDecode for ReversingDecoder {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, entries) = Entries::decode(input, &())?;
        let reversed = Entries::try_from_iter(entries.into_iter().rev())
            .expect("the same entries fit the same bounds");
        Ok((rest, Self(reversed)))
    }
}

#[test]
#[should_panic(expected = "encode(decode(bytes)) differs from the well-known bytes")]
fn the_harness_catches_a_decoder_that_reorders_what_eq_ignores() {
    assert_codec_fixtures::<ReversingDecoder>();
}
