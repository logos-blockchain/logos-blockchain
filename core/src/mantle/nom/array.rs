use nom::{IResult, Parser as _, multi::count};

use crate::mantle::nom::{NomDecode, NomEncode, encode_slice};

pub struct NomArray<'a, T, const N: usize>(&'a [T::Output; N])
where
    T: NomDecode;

impl<'a, T, const N: usize> From<&'a [T::Output; N]> for NomArray<'a, T, N>
where
    T: NomDecode,
{
    fn from(array: &'a [T::Output; N]) -> Self {
        Self(array)
    }
}

impl<T, const N: usize> NomEncode for NomArray<'_, T, N>
where
    T: NomDecode<Output: NomEncode>,
{
    fn encode(&self) -> Vec<u8> {
        encode_slice(self.0.as_slice())
    }
}

impl<T, const N: usize> NomDecode for NomArray<'_, T, N>
where
    T: NomDecode,
{
    type Output = [T::Output; N];

    fn decode(input: &[u8]) -> IResult<&[u8], Self::Output> {
        let (input, items) = count(T::decode, N).parse(input)?;

        let Ok(items) = items.try_into() else {
            panic!("Decoded `N` elements.");
        };
        Ok((input, items))
    }
}
