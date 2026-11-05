//! A runtime array encodes as an array does: its items alone, with no length.
//!
//! The length lives outside the encoding, so the decoder takes it from its
//! context, ahead of the items' own context, and reads exactly that many items
//! without reading a length first. It comes from whoever decodes, never from
//! the input, so as for an array nothing here guards against items that decode
//! without consuming input.

use lb_utils::bounded::{RuntimeArray, RuntimeMax};

use super::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError, sealed,
};

impl<T, Len> BinaryEncode for RuntimeArray<T, Len>
where
    T: BinaryEncode,
{
    fn encoded_length(&self) -> usize {
        self.iter().map(BinaryEncode::encoded_length).sum()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        for item in self.iter() {
            item.encode_into(out);
        }
    }
}

impl<T, Len> BinaryDecode for RuntimeArray<T, Len>
where
    T: BinaryDecode,
    Len: RuntimeMax,
{
    /// The number of items, then the items' context.
    type Context = (Len, T::Context);

    fn decode<'input>(
        input: &'input [u8],
        (len, context): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let len = len.to_usize();
        if len == 0 {
            return Err(DecodeError::length_out_of_bounds::<Self>(
                len,
                1,
                Len::MAX.to_usize(),
            ));
        }

        let mut rest = input;
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            let (next, item) = T::decode(rest, context)?;
            rest = next;
            items.push(item);
        }
        Ok((rest, Self::new_unchecked(items)))
    }
}

impl<T, Len> sealed::Sealed for RuntimeArray<T, Len> where T: CodecExamples {}

// The fixture is one item, `T`'s, as an array of one: it round-trips under a
// length of 1.
impl<T, Len> CodecExamples for RuntimeArray<T, Len>
where
    T: CodecExamples,
{
    fn fixtures() -> CodecFixtures<Self> {
        let item = T::fixtures()
            .into_iter()
            .next()
            .expect("`CodecExamples::fixtures` is non-empty");

        [CodecFixture {
            value: Self::new_unchecked(vec![item.value]),
            bytes: item.bytes,
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::RuntimeArray;

    use crate::canonical::{
        BinaryDecode as _, BinaryEncode as _, DecodeError, assert_codec_fixtures_with,
    };

    type Array = RuntimeArray<u16, u8>;

    fn array() -> Array {
        Array::try_new(vec![1, 2], 2).unwrap()
    }

    #[test]
    fn it_encodes_as_an_array_of_its_length() {
        assert_eq!(array().encode_to_vec(), [1u16, 2].encode_to_vec());
        assert_eq!(array().encoded_length(), 4);
    }

    /// The length's type only says what the context gives: it leaves no trace
    /// in the encoding.
    #[test]
    fn it_encodes_the_same_under_every_length_type() {
        let under_a_u32 = RuntimeArray::<u16, u32>::try_new(vec![1, 2], 2).unwrap();

        assert_eq!(under_a_u32.encode_to_vec(), array().encode_to_vec());
    }

    #[test]
    fn decoding_reads_as_many_items_as_the_context_gives() {
        let bytes = [1, 0, 2, 0, 3, 0];

        let (rest, decoded) = Array::decode(&bytes, &(2, ())).unwrap();

        assert_eq!(rest, [3, 0]);
        assert_eq!(decoded, array());
    }

    #[test]
    fn input_shorter_than_the_length_is_refused() {
        let err = Array::decode(&[1, 0], &(2, ())).unwrap_err();

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn a_length_of_zero_is_refused() {
        let err = Array::decode(&[], &(0, ())).unwrap_err();

        assert!(matches!(
            err,
            DecodeError::LengthOutOfBounds {
                len: 0,
                min: 1,
                max: 255,
                ..
            }
        ));
    }

    #[test]
    fn fixtures_roundtrip_under_their_length() {
        assert_codec_fixtures_with::<Array, _>(|| (1, ()));
    }
}
