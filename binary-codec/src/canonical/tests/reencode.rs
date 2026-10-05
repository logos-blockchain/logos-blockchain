//! The fixture harness must catch a decoder that loses what `==` ignores.

use std::borrow::Cow;

use crate::canonical::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    assert_codec_fixtures, sealed, take,
};

/// Two bytes, of which `==` compares only the first. The second is the kind of
/// detail a type may leave out of its equality while its encoding keeps it.
#[derive(Debug, Clone, Copy)]
struct Tagged {
    tag: u8,
    detail: u8,
}

impl PartialEq for Tagged {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag
    }
}

impl Eq for Tagged {}

impl sealed::Sealed for Tagged {}

// Written by hand rather than with `codec_fixtures!`, whose generated test
// would fail on this deliberately broken codec.
impl CodecExamples for Tagged {
    fn fixtures() -> CodecFixtures<Self> {
        [CodecFixture {
            value: Self { tag: 1, detail: 2 },
            bytes: Cow::Borrowed(&[1, 2]),
        }]
        .into()
    }
}

impl BinaryEncode for Tagged {
    fn encoded_length(&self) -> usize {
        2
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.tag, self.detail]);
    }
}

/// Decodes both bytes but drops the detail, which `==` cannot see.
impl BinaryDecode for Tagged {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (head, rest) = take::<Self>(input, 2)?;
        Ok((
            rest,
            Self {
                tag: head[0],
                detail: 0,
            },
        ))
    }
}

#[test]
#[should_panic(expected = "encode(decode(bytes)) differs from the well-known bytes")]
fn the_harness_catches_a_decoder_that_loses_what_eq_ignores() {
    assert_codec_fixtures::<Tagged>();
}
