use core::num::NonZero;

use lb_utils::math::{NonNegativeF64, NonNegativeRatio, PositiveF64};

use super::{BinaryEncode, codec_fixtures};

// A positive float: its IEEE 754 bits.
impl BinaryEncode for PositiveF64 {
    fn encoded_length(&self) -> usize {
        self.get().to_bits().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.get().to_bits().encode_into(out);
    }
}

codec_fixtures!(PositiveF64, encode_only, PositiveF64::try_from(0.5).unwrap() => "000000000000e03f");

// A non-negative float: its IEEE 754 bits, with negative zero written as zero,
// since the two compare equal and so must encode alike.
impl BinaryEncode for NonNegativeF64 {
    fn encoded_length(&self) -> usize {
        canonical_bits(self.get()).encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        canonical_bits(self.get()).encode_into(out);
    }
}

/// The IEEE 754 bits of `value`, with both zeros written as positive zero.
fn canonical_bits(value: f64) -> u64 {
    if value == 0.0 { 0 } else { value.to_bits() }
}

codec_fixtures!(
    NonNegativeF64,
    encode_only,
    NonNegativeF64::try_from(0.5).unwrap() => "000000000000e03f",
    NonNegativeF64::try_from(0.0).unwrap() => "0000000000000000",
    NonNegativeF64::try_from(-0.0).unwrap() => "0000000000000000"
);

// A ratio: its numerator, then its denominator.
impl BinaryEncode for NonNegativeRatio {
    fn encoded_length(&self) -> usize {
        self.numerator.encoded_length() + self.denominator.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.numerator.encode_into(out);
        self.denominator.encode_into(out);
    }
}

codec_fixtures!(
    NonNegativeRatio,
    encode_only,
    NonNegativeRatio::new(1, NonZero::<u32>::new(2).unwrap()) => "0100000002000000"
);
