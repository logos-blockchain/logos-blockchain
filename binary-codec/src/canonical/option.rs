use std::borrow::Cow;

use super::{BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, sealed};

#[repr(u8)]
enum OptionTag {
    None = 0,
    Some = 1,
}

impl OptionTag {
    fn encoded_length() -> usize {
        (Self::None as u8).encoded_length()
    }
}

impl<T> BinaryEncode for Option<T>
where
    T: BinaryEncode,
{
    fn encoded_length(&self) -> usize {
        OptionTag::encoded_length() + self.as_ref().map_or(0, BinaryEncode::encoded_length)
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            None => (OptionTag::None as u8).encode_into(out),
            Some(value) => {
                (OptionTag::Some as u8).encode_into(out);
                value.encode_into(out);
            }
        }
    }
}

impl<T> sealed::Sealed for Option<T> where T: CodecExamples {}

// The fixtures are `None`, then `Some` of each of the value's own fixtures, so
// every `Option` monomorphization has fixtures without writing any.
impl<T> CodecExamples for Option<T>
where
    T: CodecExamples,
{
    fn fixtures() -> CodecFixtures<Self> {
        let mut fixtures = vec![CodecFixture {
            value: None,
            bytes: Cow::Borrowed(&[0]),
        }];
        for fixture in T::fixtures() {
            let mut bytes = vec![1];
            bytes.extend_from_slice(fixture.bytes.as_ref());
            fixtures.push(CodecFixture {
                value: Some(fixture.value),
                bytes: Cow::Owned(bytes),
            });
        }
        fixtures
            .try_into()
            .expect("`None` makes the fixtures non-empty")
    }
}

#[cfg(test)]
mod tests {
    use crate::canonical::{BinaryEncode as _, assert_codec_fixtures_encode_only};

    #[test]
    fn fixtures_hold() {
        assert_codec_fixtures_encode_only::<Option<u8>>();
    }

    #[test]
    fn none_is_a_zero_byte_and_some_a_one_byte_before_the_value() {
        assert_eq!(None::<u8>.encode_to_vec(), [0]);
        assert_eq!(Some(7u8).encode_to_vec(), [1, 7]);
    }
}
