use lb_groth16::{AdditiveGroup as _, Fr, Groth16Input};
use lb_log_targets::kms;
use lb_utils::bounded::UpperBoundedVec;
use lb_zksign::{ZkSignError, ZkSignVerifierInputs};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::keys::zk::{private::SecretKey, signature::Signature};

const LOG_TARGET: &str = kms::keys::ZK;

pub const MAX_ZK_SIGNING_KEYS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PublicKey(#[serde(with = "lb_groth16::serde::serde_fr")] Fr);

pub type PublicKeys = UpperBoundedVec<PublicKey, MAX_ZK_SIGNING_KEYS>;
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

    #[must_use]
    pub fn verify_multi(pks: &[Self], data: &Fr, signature: &Signature) -> bool {
        let inputs = match inputs_from_pks((*data).into(), pks) {
            Ok(inputs) => inputs,
            Err(e) => {
                error!(target: LOG_TARGET, "Error building verifier inputs: {e:?}");
                return false;
            }
        };

        lb_zksign::verify(signature.as_proof(), &inputs).unwrap_or_else(|e| {
            error!(target: LOG_TARGET, "Error verifying signature: {e:?}");
            false
        })
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

pub fn inputs_from_pks(
    msg: Groth16Input,
    pks: &[PublicKey],
) -> Result<ZkSignVerifierInputs, ZkSignError> {
    if pks.len() > MAX_ZK_SIGNING_KEYS {
        return Err(ZkSignError::TooManyKeys(pks.len()));
    }

    // pks are padded with the pk corresponding to the zero SecretKey.
    let zero_pk = Groth16Input::from(SecretKey::zero().to_public_key().into_inner());
    let mut public_keys = [zero_pk; MAX_ZK_SIGNING_KEYS];

    for (i, pk) in pks.iter().enumerate() {
        assert!(
            i < MAX_ZK_SIGNING_KEYS,
            "ZkSign supports signing with at most {MAX_ZK_SIGNING_KEYS} keys"
        );
        public_keys[i] = Groth16Input::from(pk.into_inner());
    }

    Ok(ZkSignVerifierInputs { public_keys, msg })
}
