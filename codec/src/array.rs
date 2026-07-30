use std::borrow::Cow;

use crate::{BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, sealed};

// Fixed-size array: `N` elements concatenated with NO length prefix — `N` lives
// in the type, not in its encoding.
impl<T, const N: usize> BinaryEncode for [T; N]
where
    T: BinaryEncode,
{
    fn encoded_length(&self) -> usize {
        self.iter().map(BinaryEncode::encoded_length).sum()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        for item in self {
            item.encode_into(out);
        }
    }
}

impl<T, const N: usize> BinaryDecode for [T; N]
where
    T: BinaryDecode,
{
    type Context = T::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), crate::DecodeError> {
        let mut rest = input;
        let mut items = Vec::with_capacity(N);
        for _ in 0..N {
            let (next, item) = T::decode(rest, context)?;
            rest = next;
            items.push(item);
        }

        let array = <[T; N]>::try_from(items)
            .unwrap_or_else(|_| unreachable!("decoded exactly `N` elements"));
        Ok((rest, array))
    }
}

impl<T, const N: usize> sealed::Sealed for [T; N] where T: CodecExamples {}

// Like the `BoundedVec` blanket but with no length prefix — `N` lives in the
// type. `N` elements built from `T`'s fixture; bound stays at `T:
// CodecExamples` (no `Clone`) so the supertrait requirement goes through.
impl<T, const N: usize> CodecExamples for [T; N]
where
    T: CodecExamples,
{
    fn fixtures() -> CodecFixtures<Self> {
        let mut bytes = Vec::new();
        let value = core::array::from_fn(|_| {
            let item = T::fixtures()
                .into_iter()
                .next()
                .expect("`CodecExamples::fixtures` is non-empty");
            bytes.extend_from_slice(item.bytes.as_ref());
            item.value
        });

        [CodecFixture {
            value,
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}
