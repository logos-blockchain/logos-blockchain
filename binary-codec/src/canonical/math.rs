use core::num::NonZero;

use lb_utils::math::{NonNegativeF64, NonNegativeRatio, PositiveRatio, ProperFraction};

use super::{BinaryEncode, codec_fixtures};

// A non-negative float: its IEEE 754 bits. Negative zero is no non-negative
// float, so zero encodes one way.
impl BinaryEncode for NonNegativeF64 {
    fn encoded_length(&self) -> usize {
        self.get().to_bits().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.get().to_bits().encode_into(out);
    }
}

codec_fixtures!(
    NonNegativeF64,
    encode_only,
    NonNegativeF64::try_from(0.5).unwrap() => "000000000000e03f",
    NonNegativeF64::try_from(0.0).unwrap() => "0000000000000000"
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

// A fraction: the ratio it is.
impl BinaryEncode for ProperFraction {
    fn encoded_length(&self) -> usize {
        self.get().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.get().encode_into(out);
    }
}

codec_fixtures!(ProperFraction, encode_only, ProperFraction::new(1, 2.try_into().unwrap()).unwrap() => "0100000002000000");

// A positive ratio: the ratio it is.
impl BinaryEncode for PositiveRatio {
    fn encoded_length(&self) -> usize {
        self.get().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.get().encode_into(out);
    }
}

codec_fixtures!(PositiveRatio, encode_only, PositiveRatio::new(3, 2.try_into().unwrap()).unwrap() => "0300000002000000");
