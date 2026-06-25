use std::borrow::Cow;

use nom::{IResult, Parser as _, multi::count};

use crate::mantle::nom::{
    NomDecode, NomEncode, WireExamples, WireFixture, WireFixtures, encode_slice, sealed,
};

impl<T, const N: usize> NomEncode for [T; N]
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        encode_slice(self.as_slice())
    }
}

impl<T, const N: usize> NomDecode for [T; N]
where
    T: NomDecode,
{
    fn decode(input: &[u8]) -> IResult<&[u8], Self> {
        let (input, items) = count(T::decode, N).parse_complete(input)?;

        let Ok(items) = items.try_into() else {
            panic!("Decoded `N` elements.");
        };
        Ok((input, items))
    }
}

impl<T, const N: usize> sealed::Sealed for [T; N] where T: WireExamples {}

// Like the `BoundedVec` blanket but with no length prefix — `N` lives in the
// type, not on the wire. `N` elements built from `T`'s fixture; bound stays at
// `T: WireExamples` (no `Clone`) so the supertrait flip goes through.
impl<T, const N: usize> WireExamples for [T; N]
where
    T: WireExamples,
{
    fn fixtures() -> WireFixtures<Self> {
        let mut bytes = Vec::new();
        let value = ::core::array::from_fn(|_| {
            let item = T::fixtures()
                .into_iter()
                .next()
                .expect("`WireExamples::fixtures` is non-empty");
            bytes.extend_from_slice(item.bytes.as_ref());
            item.value
        });

        [WireFixture {
            value,
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}
