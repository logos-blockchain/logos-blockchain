use core::num::NonZero;

use lb_groth16::{Fr, ModulusShift, fr_from_bytes, fr_to_bytes};

use super::{BinaryDecode, BinaryEncode, DecodeError, codec_fixtures, take};
/// `BinaryEncode`/`BinaryDecode` for a little-endian fixed-width integer.
macro_rules! impl_le_integer {
    ($ty:ty) => {
        impl BinaryEncode for $ty {
            fn encoded_length(&self) -> usize {
                ::core::mem::size_of::<$ty>()
            }

            fn encode_into(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
        }

        impl BinaryDecode for $ty {
            type Context = ();

            fn decode<'input>(
                input: &'input [u8],
                (): &Self::Context,
            ) -> Result<(&'input [u8], Self), DecodeError> {
                let (head, rest) = take::<Self>(input, ::core::mem::size_of::<$ty>())?;
                let value =
                    <$ty>::from_le_bytes(head.try_into().expect("take took the right length"));
                Ok((rest, value))
            }
        }
    };
}

impl_le_integer!(u8);
impl_le_integer!(u16);
impl_le_integer!(u32);
impl_le_integer!(u64);
impl_le_integer!(u128);

codec_fixtures!(u8, 0x07u8 => "07", 0u8 => "00");
codec_fixtures!(u16, 1u16 => "0100", 0x0201u16 => "0102");
codec_fixtures!(u32, 1u32 => "01000000", 0x0403_0201u32 => "01020304");
codec_fixtures!(u64, 1u64 => "0100000000000000", 0x0807_0605_0403_0201u64 => "0102030405060708");
codec_fixtures!(
    u128,
    1u128 => "01000000000000000000000000000000",
    0x100f_0e0d_0c0b_0a09_0807_0605_0403_0201u128 => "0102030405060708090a0b0c0d0e0f10"
);

/// `BinaryEncode` for a non-zero integer: the integer it wraps.
macro_rules! impl_non_zero {
    ($ty:ty) => {
        impl BinaryEncode for NonZero<$ty> {
            fn encoded_length(&self) -> usize {
                self.get().encoded_length()
            }

            fn encode_into(&self, out: &mut Vec<u8>) {
                self.get().encode_into(out);
            }
        }
    };
}

impl_non_zero!(u8);
impl_non_zero!(u16);
impl_non_zero!(u32);
impl_non_zero!(u64);
impl_non_zero!(u128);

codec_fixtures!(NonZero<u8>, encode_only, NonZero::<u8>::new(0x07).unwrap() => "07");
codec_fixtures!(NonZero<u16>, encode_only, NonZero::<u16>::new(0x0201).unwrap() => "0102");
codec_fixtures!(NonZero<u32>, encode_only, NonZero::<u32>::new(0x0403_0201).unwrap() => "01020304");
codec_fixtures!(
    NonZero<u64>,
    encode_only,
    NonZero::<u64>::new(0x0807_0605_0403_0201).unwrap() => "0102030405060708"
);
codec_fixtures!(
    NonZero<u128>,
    encode_only,
    NonZero::<u128>::new(1).unwrap() => "01000000000000000000000000000000"
);

// A modulus shift: the exponent it wraps.
impl BinaryEncode for ModulusShift {
    fn encoded_length(&self) -> usize {
        self.get().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.get().encode_into(out);
    }
}

codec_fixtures!(ModulusShift, encode_only, ModulusShift::new::<19>() => "13000000");

// A BLS scalar, encoded as its 32-byte little-endian representation.
impl BinaryEncode for Fr {
    fn encoded_length(&self) -> usize {
        32
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&fr_to_bytes(self));
    }
}

impl BinaryDecode for Fr {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (head, rest) = take::<Self>(input, 32)?;
        let bytes: [u8; 32] = head.try_into().expect("take took the right length");
        let value = fr_from_bytes(&bytes)
            .map_err(|_| DecodeError::invalid_value::<Self>("not a canonical field element"))?;
        Ok((rest, value))
    }
}

codec_fixtures!(
    Fr,
    Self::from(1u64) => "0100000000000000000000000000000000000000000000000000000000000000"
);
