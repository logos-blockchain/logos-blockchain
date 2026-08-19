//! Field elements written as a fraction of the scalar field.

use core::fmt::{self, Debug, Display, Formatter};

use ark_ff::PrimeField as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Fr, fr_modulus};

/// A field element written as the fraction `p / 2^n` of the scalar field.
///
/// Some protocol values are naturally stated as a share of the field rather
/// than as a number — a threshold a hash must fall below, say, where what
/// matters is the fraction of the field it admits, and where the specification
/// itself reasons in exponents. Carrying the exponent keeps that reading, and
/// keeps a deployment from writing a 32-byte value that is not a sane fraction
/// of the field at all.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct ModulusShift(u32);

impl ModulusShift {
    /// The largest shift that still denotes a non-zero element: `p` is
    /// [`Fr::MODULUS_BIT_SIZE`] bits wide, so shifting by that many discards
    /// every one of them.
    pub const MAX: u32 = Fr::MODULUS_BIT_SIZE - 1;

    /// Builds a value from a constant, refusing to compile if it does not
    /// denote a field element.
    ///
    /// Prefer this over [`Self::try_new`] wherever the exponent is a literal
    /// or a `const`, so that an out-of-range constant is a build failure
    /// rather than a startup failure.
    #[must_use]
    pub const fn new<const SHIFT: u32>() -> Self {
        const {
            assert!(
                SHIFT >= 1 && SHIFT <= Self::MAX,
                "shift does not denote a field element"
            );
        }
        Self(SHIFT)
    }

    /// Builds a value that is only known at runtime — a deployment setting,
    /// typically — returning [`Err`] if it does not denote a field element.
    pub const fn try_new(shift: u32) -> Result<Self, ModulusShiftOutOfRange> {
        if shift >= 1 && shift <= Self::MAX {
            Ok(Self(shift))
        } else {
            Err(ModulusShiftOutOfRange { shift })
        }
    }

    /// The exponent `n` itself.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<ModulusShift> for Fr {
    fn from(shift: ModulusShift) -> Self {
        (fr_modulus() >> shift.0).into()
    }
}

impl TryFrom<u32> for ModulusShift {
    type Error = ModulusShiftOutOfRange;

    fn try_from(shift: u32) -> Result<Self, Self::Error> {
        Self::try_new(shift)
    }
}

impl From<ModulusShift> for u32 {
    fn from(shift: ModulusShift) -> Self {
        shift.get()
    }
}

impl Display for ModulusShift {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "p/2^{}", self.0)
    }
}

impl Debug for ModulusShift {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "ModulusShift({self})")
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("Modulus shift {shift} does not denote a field element: it must be within 1..={max}", max = ModulusShift::MAX)]
pub struct ModulusShiftOutOfRange {
    pub shift: u32,
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;

    use super::*;
    use crate::fr_to_bytes;

    fn as_int(value: Fr) -> BigUint {
        BigUint::from_bytes_le(&fr_to_bytes(&value))
    }

    #[test]
    fn denotes_the_field_over_two_to_the_shift() {
        assert_eq!(
            as_int(Fr::from(ModulusShift::new::<19>())),
            fr_modulus() >> 19
        );
        assert_eq!(
            as_int(Fr::from(ModulusShift::new::<1>())),
            fr_modulus() >> 1
        );
    }

    #[test]
    fn the_maximum_shift_still_denotes_a_non_zero_element() {
        // One more bit would leave nothing, which is what `MAX` marks.
        assert!(as_int(Fr::from(ModulusShift::new::<{ ModulusShift::MAX }>())) > BigUint::ZERO);
        assert_eq!(fr_modulus() >> (ModulusShift::MAX + 1), BigUint::ZERO);
    }

    #[test]
    fn a_shift_of_zero_is_refused() {
        // `p` is not a field element, and as a threshold it admits everything.
        assert_eq!(
            ModulusShift::try_new(0),
            Err(ModulusShiftOutOfRange { shift: 0 })
        );
    }

    #[test]
    fn a_shift_past_the_field_is_refused() {
        // Denotes zero, which as a threshold admits nothing.
        let past_the_field = ModulusShift::MAX + 1;
        assert_eq!(
            ModulusShift::try_new(past_the_field),
            Err(ModulusShiftOutOfRange {
                shift: past_the_field
            })
        );
    }

    #[test]
    fn deserialization_refuses_a_shift_that_denotes_nothing() {
        // The check must bite where a deployment value enters the process.
        assert_eq!(
            serde_json::from_str::<ModulusShift>("19").unwrap(),
            ModulusShift::new::<19>()
        );
        assert!(serde_json::from_str::<ModulusShift>("0").is_err());
        assert!(
            serde_json::from_str::<ModulusShift>(&(ModulusShift::MAX + 1).to_string()).is_err()
        );
    }

    #[test]
    fn round_trips_as_a_plain_number() {
        let shift = ModulusShift::new::<19>();
        assert_eq!(serde_json::to_string(&shift).unwrap(), "19");
    }
}
