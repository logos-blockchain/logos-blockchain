use groth16::{Field as _, Fr, Groth16Input};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use tracing::error;
use zksign::{ZkSignError, ZkSignVerifierInputs};

use crate::keys::zk::{private::SecretKey, signature::Signature};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PublicKey(#[serde(with = "groth16::serde::serde_fr")] Fr);

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
        let inputs = match try_from_pks((*data).into(), pks) {
            Ok(inputs) => inputs,
            Err(e) => {
                error!("Error building verifier inputs: {e:?}");
                return false;
            }
        };

        zksign::verify(signature.as_proof(), &inputs).unwrap_or_else(|e| {
            error!("Error verifying signature: {e:?}");
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

fn try_from_pks(msg: Groth16Input, pks: &[PublicKey]) -> Result<ZkSignVerifierInputs, ZkSignError> {
    if pks.len() > 32 {
        return Err(ZkSignError::TooManyKeys(pks.len()));
    }

    // pks are padded with the pk corresponding to the zero SecretKey.
    let zero_pk = Groth16Input::from(SecretKey::zero().to_public_key().into_inner());
    let mut public_keys = [zero_pk; 32];

    for (i, pk) in pks.iter().enumerate() {
        assert!(i < 32, "ZkSign supports signing with at most 32 keys");
        public_keys[i] = Groth16Input::from(pk.into_inner());
    }

    Ok(ZkSignVerifierInputs { public_keys, msg })
}
