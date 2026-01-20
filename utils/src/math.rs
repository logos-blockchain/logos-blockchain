use core::ops::{Deref, DerefMut};

#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub struct FiniteF64(
    #[cfg_attr(feature = "serde", serde(deserialize_with = "serde::deserialize"))] f64,
);

/// A wrapper around [`f64`] that guarantees the value is neither infinite nor
/// NaN.
impl FiniteF64 {
    /// The maximum [`u64`] value that can be represented exactly as an [`f64`]
    /// without losing precision.
    const MAX_REPRESENTABLE_U64: u64 = (1u64 << f64::MANTISSA_DIGITS) - 1;

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Deref for FiniteF64 {
    type Target = f64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for FiniteF64 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TryFrom<f64> for FiniteF64 {
    type Error = ();

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(())
        }
    }
}

impl TryFrom<u64> for FiniteF64 {
    type Error = ();

    /// Converts [`u64`] to [`FiniteF64`] if it can be represented exactly.
    /// Returns an error if precision would be lost.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value > Self::MAX_REPRESENTABLE_U64 {
            return Err(());
        }
        Self::try_from(value as f64)
    }
}

/// A wrapper around [`FiniteF64`] that guarantees the value is >= 0.0.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub struct NonNegativeF64(
    #[cfg_attr(
        feature = "serde",
        serde(deserialize_with = "serde::deserialize_non_negative")
    )]
    FiniteF64,
);

impl NonNegativeF64 {
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0.get()
    }
}

impl Deref for NonNegativeF64 {
    type Target = f64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for NonNegativeF64 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TryFrom<f64> for NonNegativeF64 {
    type Error = ();

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        FiniteF64::try_from(value)?.try_into()
    }
}

impl TryFrom<FiniteF64> for NonNegativeF64 {
    type Error = ();

    fn try_from(value: FiniteF64) -> Result<Self, Self::Error> {
        if value.0 >= 0.0 {
            Ok(Self(value))
        } else {
            Err(())
        }
    }
}

impl TryFrom<u64> for NonNegativeF64 {
    type Error = ();

    /// Converts [`u64`] to [`NonNegativeF64`] if it can be represented exactly.
    /// Returns an error if precision would be lost or the value is negative.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        FiniteF64::try_from(value)?.try_into()
    }
}

/// A wrapper around [`NonNegativeF64`] that guarantees the value is > 0.0
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub struct PositiveF64(
    #[cfg_attr(
        feature = "serde",
        serde(deserialize_with = "serde::deserialize_positive")
    )]
    NonNegativeF64,
);

impl PositiveF64 {
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0.get()
    }

    #[must_use]
    pub fn log2(&self) -> FiniteF64 {
        self.0
            .log2()
            .try_into()
            .expect("log2 of positive number must be neither infinite nor NaN")
    }
}

impl Deref for PositiveF64 {
    type Target = f64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PositiveF64 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TryFrom<f64> for PositiveF64 {
    type Error = ();

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        let non_negative = NonNegativeF64::try_from(value)?;
        Self::try_from(non_negative)
    }
}

impl TryFrom<NonNegativeF64> for PositiveF64 {
    type Error = ();

    fn try_from(value: NonNegativeF64) -> Result<Self, Self::Error> {
        if value.get() == 0.0 {
            Err(())
        } else {
            Ok(Self(value))
        }
    }
}

impl TryFrom<u64> for PositiveF64 {
    type Error = ();

    /// Converts [`u64`] to [`PositiveF64`] if it can be represented exactly.
    /// Returns an error if precision would be lost or the value is < 0.0.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        NonNegativeF64::try_from(value)?.try_into()
    }
}

/// A wrapper around [`PositiveF64`] that guarantees the value is >= 1.0
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub struct F64Ge1(
    #[cfg_attr(feature = "serde", serde(deserialize_with = "serde::deserialize_ge1"))] PositiveF64,
);

impl F64Ge1 {
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0.get()
    }

    #[must_use]
    pub fn log2(&self) -> NonNegativeF64 {
        self.0
            .log2()
            .try_into()
            .expect("log2 of value >= 1.0 must be non-negative")
    }
}

impl Deref for F64Ge1 {
    type Target = f64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<f64> for F64Ge1 {
    type Error = ();

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        let positive = PositiveF64::try_from(value)?;
        Self::try_from(positive)
    }
}

impl TryFrom<PositiveF64> for F64Ge1 {
    type Error = ();

    fn try_from(value: PositiveF64) -> Result<Self, Self::Error> {
        if value.get() >= 1.0 {
            Ok(Self(value))
        } else {
            Err(())
        }
    }
}

impl TryFrom<u64> for F64Ge1 {
    type Error = ();

    /// Converts [`u64`] to [`F64Ge1`] if it can be represented exactly.
    /// Returns an error if precision would be lost or the value is < 1.0.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        PositiveF64::try_from(value)?.try_into()
    }
}

#[cfg(feature = "serde")]
mod serde {
    use serde::Deserialize as _;

    use crate::math::{F64Ge1, FiniteF64, NonNegativeF64, PositiveF64};

    pub(super) fn deserialize<'de, Deserializer>(
        deserializer: Deserializer,
    ) -> Result<f64, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let inner = f64::deserialize(deserializer)?;
        NonNegativeF64::try_from(inner).map_err(|()| {
            serde::de::Error::custom("Deserialized f64 does not contain a valid value.")
        })?;
        Ok(inner)
    }

    pub(super) fn deserialize_non_negative<'de, Deserializer>(
        deserializer: Deserializer,
    ) -> Result<FiniteF64, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let inner = FiniteF64::deserialize(deserializer)?;
        NonNegativeF64::try_from(inner)
            .map_err(|()| serde::de::Error::custom("Deserialized f64 must be non-negative."))?;
        Ok(inner)
    }

    pub(super) fn deserialize_positive<'de, Deserializer>(
        deserializer: Deserializer,
    ) -> Result<NonNegativeF64, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let inner = NonNegativeF64::deserialize(deserializer)?;
        PositiveF64::try_from(inner)
            .map_err(|()| serde::de::Error::custom("Deserialized f64 must be positive."))?;
        Ok(inner)
    }

    pub(super) fn deserialize_ge1<'de, Deserializer>(
        deserializer: Deserializer,
    ) -> Result<PositiveF64, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let inner = PositiveF64::deserialize(deserializer)?;
        F64Ge1::try_from(inner)
            .map_err(|()| serde::de::Error::custom("Deserialized f64 must be >= 1.0"))?;
        Ok(inner)
    }
}

#[cfg(test)]
mod tests {
    use std::f64;

    use super::*;

    #[test]
    fn finite_basic() {
        assert!(FiniteF64::try_from(0.0).is_ok());
        assert!(FiniteF64::try_from(1.0).is_ok());
        assert!(FiniteF64::try_from(-1.0).is_ok());
        assert!(FiniteF64::try_from(f64::INFINITY).is_err());
        assert!(FiniteF64::try_from(f64::NAN).is_err());
    }

    #[test]
    fn finite_from_u64() {
        assert!(FiniteF64::try_from(0u64).is_ok());
        assert!(FiniteF64::try_from(1u64).is_ok());

        assert!(FiniteF64::try_from(FiniteF64::MAX_REPRESENTABLE_U64).is_ok());
        assert!(FiniteF64::try_from(FiniteF64::MAX_REPRESENTABLE_U64 + 1).is_err());
    }

    #[test]
    fn non_negative_basic() {
        assert!(NonNegativeF64::try_from(0.0).is_ok());
        assert!(NonNegativeF64::try_from(1.0).is_ok());
        assert!(NonNegativeF64::try_from(-0.1).is_err());
    }

    #[test]
    fn positive_basic() {
        assert!(PositiveF64::try_from(0.1).is_ok());
        assert!(PositiveF64::try_from(0.0).is_err());
        assert!(PositiveF64::try_from(-1.0).is_err());
    }

    #[test]
    fn f64ge1_basic() {
        assert!(F64Ge1::try_from(1.0).is_ok());
        assert!(F64Ge1::try_from(2.0).is_ok());
        assert!(F64Ge1::try_from(0.9).is_err());
    }

    #[test]
    fn f64ge1_from_u64() {
        assert!(F64Ge1::try_from(0u64).is_err());
        assert!(F64Ge1::try_from(1u64).is_ok());
        assert!(F64Ge1::try_from(2u64).is_ok());
        assert!(F64Ge1::try_from(FiniteF64::MAX_REPRESENTABLE_U64).is_ok());
        assert!(F64Ge1::try_from(FiniteF64::MAX_REPRESENTABLE_U64 + 1).is_err());
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use ::serde::Deserialize;

        use super::*;

        #[derive(Debug, Deserialize)]
        struct Finite {
            value: FiniteF64,
        }
        #[derive(Debug, Deserialize)]
        struct NonNegative {
            value: NonNegativeF64,
        }
        #[derive(Debug, Deserialize)]
        struct Positive {
            value: PositiveF64,
        }
        #[derive(Debug, Deserialize)]
        struct Ge1 {
            value: F64Ge1,
        }

        #[test]
        fn deser_finite() {
            let ok: Finite = serde_json::from_str(r#"{"value": 0.1}"#).unwrap();
            assert!((*ok.value - 0.1).abs() < f64::EPSILON);

            assert!(serde_json::from_str::<Finite>(r#"{"value": 0.0}"#).is_ok());
            assert!(serde_json::from_str::<Finite>(r#"{"value": -1.0}"#).is_err());
        }

        #[test]
        fn deser_non_negativef64() {
            let ok: NonNegative = serde_json::from_str(r#"{"value": 0.1}"#).unwrap();
            assert!((*ok.value - 0.1).abs() < f64::EPSILON);

            assert!(serde_json::from_str::<NonNegative>(r#"{"value": 0.0}"#).is_ok());
            assert!(serde_json::from_str::<NonNegative>(r#"{"value": -1.0}"#).is_err());
        }

        #[test]
        fn deser_positivef64() {
            let ok: Positive = serde_json::from_str(r#"{"value": 0.1}"#).unwrap();
            assert!((*ok.value - 0.1).abs() < f64::EPSILON);

            assert!(serde_json::from_str::<Positive>(r#"{"value": 0.0}"#).is_err());
            assert!(serde_json::from_str::<Positive>(r#"{"value": -1.0}"#).is_err());
        }

        #[test]
        fn deser_f64ge1() {
            let ok: Ge1 = serde_json::from_str(r#"{"value": 1.0}"#).unwrap();
            assert!((*ok.value - 1.0).abs() < f64::EPSILON);

            assert!(serde_json::from_str::<F64Ge1>(r#"{"value": 1.1}"#).is_err());
        }
    }
}
