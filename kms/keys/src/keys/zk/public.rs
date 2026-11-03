use lb_groth16::{AdditiveGroup as _, Fr};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PublicKey(#[serde(with = "lb_groth16::serde::serde_fr")] Fr);

// Encoded through `serde_fr`, which routes to
// `lb_utils::serde::serialize_bytes_array`, so the key documents itself with
// that encoding's schema rather than restating it at each use site.
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for PublicKey {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        lb_utils::openapi::hex_bytes_schema(size_of::<lb_groth16::FrBytes>())
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for PublicKey {}

impl PublicKey {
    #[must_use]
    pub const fn zero() -> Self {
        Self(Fr::ZERO)
    }

    #[must_use]
    pub const fn new(key: Fr) -> Self {
        Self(key)
    }

    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        &self.0
    }

    #[must_use]
    pub const fn into_inner(self) -> Fr {
        self.0
    }
}

impl From<PublicKey> for Fr {
    fn from(value: PublicKey) -> Self {
        value.0
    }
}

impl From<BigUint> for PublicKey {
    fn from(value: BigUint) -> Self {
        Fr::from(value).into()
    }
}

impl From<Fr> for PublicKey {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}
