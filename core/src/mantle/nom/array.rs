use nom::{IResult, Parser as _, multi::count};

use crate::mantle::nom::{NomDecode, NomEncode, encode_slice};

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
