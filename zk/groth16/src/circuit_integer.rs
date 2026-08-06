//! Unsigned integers whose width is fixed by the circuit signal they feed.

use core::fmt::{self, Debug, Display, Formatter};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Fr, Groth16Input};

/// An unsigned integer constrained to `0..=2^BITS - 1`.
///
/// The bound is an invariant of the type: every value of
/// `CircuitInteger<BITS>` fits in `BITS` bits, so conversions into circuit
/// inputs are infallible. Constants are checked when they are compiled
/// ([`Self::new_const`]); only values that are genuinely unknown until runtime
/// need [`Self::new`].
///
/// Circuits are expected to alias this type at the width of the signal they
/// feed, so that widening or narrowing a signal is a one-line change that
/// surfaces every invalidated constant as a build failure.
#[derive(Copy, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct CircuitInteger<const BITS: u8>(u64);

impl<const BITS: u8> CircuitInteger<BITS> {
    /// Widths outside `1..=64` cannot be represented by the backing store.
    /// [`Self::MAX`] forces this to be evaluated, so an invalid width is a
    /// compile error at the point the type is used rather than a silent
    /// overflow.
    const WIDTH_IS_VALID: () = assert!(
        BITS >= 1 && BITS as u32 <= u64::BITS,
        "BITS must be within 1..=64"
    );

    /// The largest representable value, `2^BITS - 1`.
    pub const MAX: u64 = {
        let () = Self::WIDTH_IS_VALID;
        // Not `(1 << BITS) - 1`, which overflows at `BITS == 64`.
        u64::MAX >> (64 - BITS)
    };

    /// The smallest representable value.
    pub const ZERO: Self = Self::new::<0>();

    /// Builds a value from a constant, refusing to compile if it does not fit
    /// in `BITS` bits.
    ///
    /// Prefer this over [`Self::new`] wherever the value is a literal or a
    /// `const`, so that changing the width of a signal turns every constant
    /// that no longer fits into a build failure.
    #[must_use]
    pub const fn new<const VALUE: u64>() -> Self {
        const { assert!(VALUE <= Self::MAX, "value does not fit in BITS bits") }
        Self(VALUE)
    }

    /// Builds a value that is only known at runtime, returning [`Err`] if it
    /// does not fit in `BITS` bits.
    pub const fn try_new(value: u64) -> Result<Self, CircuitIntegerOutOfRange> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(CircuitIntegerOutOfRange { value, bits: BITS })
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<const BITS: u8> TryFrom<u64> for CircuitInteger<BITS> {
    type Error = CircuitIntegerOutOfRange;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<const BITS: u8> From<CircuitInteger<BITS>> for u64 {
    fn from(value: CircuitInteger<BITS>) -> Self {
        value.0
    }
}

impl<const BITS: u8> From<CircuitInteger<BITS>> for Fr {
    fn from(value: CircuitInteger<BITS>) -> Self {
        Self::from(value.0)
    }
}

impl<const BITS: u8> From<CircuitInteger<BITS>> for Groth16Input {
    fn from(value: CircuitInteger<BITS>) -> Self {
        Self::new(value.into())
    }
}

impl<const BITS: u8> Debug for CircuitInteger<BITS> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl<const BITS: u8> Display for CircuitInteger<BITS> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("value {value} does not fit in {bits} bits")]
pub struct CircuitIntegerOutOfRange {
    value: u64,
    bits: u8,
}

#[cfg(test)]
mod tests {
    use crate::CircuitInteger;

    type Twenty = CircuitInteger<20>;

    #[test]
    fn max_is_two_to_the_bits_minus_one() {
        assert_eq!(CircuitInteger::<1>::MAX, 1);
        assert_eq!(Twenty::MAX, (1 << 20) - 1);
        // `(1 << BITS) - 1` would overflow here.
        assert_eq!(CircuitInteger::<64>::MAX, u64::MAX);
    }

    #[test]
    fn only_values_within_the_width_are_accepted() {
        assert_eq!(
            Twenty::try_new(Twenty::MAX).map(Twenty::get),
            Ok(Twenty::MAX)
        );
        assert!(Twenty::try_new(Twenty::MAX + 1).is_err());
        assert_eq!(Twenty::ZERO.get(), 0);
    }

    #[test]
    fn deserialization_rejects_out_of_range_values() {
        assert_eq!(
            serde_json::from_str::<Twenty>(&Twenty::MAX.to_string()).unwrap(),
            Twenty::new::<{ (1 << 20) - 1 }>()
        );
        assert!(serde_json::from_str::<Twenty>(&(Twenty::MAX + 1).to_string()).is_err());
    }

    #[test]
    fn serialization_round_trips_as_a_plain_integer() {
        assert_eq!(serde_json::to_string(&Twenty::new::<15>()).unwrap(), "15");
    }
}
