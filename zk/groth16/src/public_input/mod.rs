#[cfg(feature = "deser")]
use std::str::FromStr;

use ark_bn254::{Bn254, Fr};
use ark_ec::pairing::Pairing;
#[cfg(feature = "deser")]
use ark_ff::Zero as _;

#[cfg(feature = "deser")]
pub mod deserialize;

#[cfg(feature = "deser")]
pub use deserialize::InputDeser;

#[derive(Copy, Clone, Debug)]
pub struct Input<E: Pairing>(<E as Pairing>::ScalarField);

impl<E: Pairing> Input<E> {
    pub const fn new(value: E::ScalarField) -> Self {
        Self(value)
    }

    pub const fn into_inner(self) -> E::ScalarField {
        self.0
    }
}

impl<E: Pairing> AsRef<E::ScalarField> for Input<E> {
    fn as_ref(&self) -> &E::ScalarField {
        &self.0
    }
}

impl From<Fr> for Input<Bn254> {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

#[cfg(feature = "deser")]
impl TryFrom<InputDeser> for Input<Bn254> {
    type Error = <<Bn254 as Pairing>::ScalarField as FromStr>::Err;

    fn try_from(value: InputDeser) -> Result<Self, Self::Error> {
        Ok(Self(<Bn254 as Pairing>::ScalarField::from_str(
            value.0.as_str(),
        )?))
    }
}

#[cfg(feature = "deser")]
impl From<&Input<Bn254>> for InputDeser {
    fn from(value: &Input<Bn254>) -> Self {
        // Have to branch here, as by default it's an empty string, but we want "0"
        if value.0.is_zero() {
            return Self("0".to_owned());
        }
        Self(value.0.to_string())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "deser")]
    use ark_ff::Field as _;

    #[cfg(feature = "deser")]
    use super::*;
    #[cfg(feature = "deser")]
    #[test]
    fn serialize_zero() {
        let zero: Input<Bn254> = Fr::ZERO.into();
        let value: InputDeser = (&zero).into();
        assert_eq!(value.0, "0");
    }
}
