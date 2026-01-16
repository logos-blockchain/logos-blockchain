use std::sync::LazyLock;

use groth16::{Field as _, Fr, fr_from_bytes_unchecked};
use num_bigint::BigUint;
use poseidon2::{Digest, Poseidon2Bn254Hasher};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use zeroize::ZeroizeOnDrop;
use zksign::{ZkSignError, ZkSignPrivateKeysData, ZkSignWitnessInputs};

use crate::keys::zk::{public::PublicKey, signature::Signature};

static KDF: LazyLock<Fr> = LazyLock::new(|| fr_from_bytes_unchecked(b"KDF"));

/// A ZK secret key exposing methods to retrieve its inner secret value.
///
/// To be used in contexts where a KMS-like key is required, but it's not
/// possible to go through the KMS roundtrip of executing operators.
#[derive(ZeroizeOnDrop, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretKey(#[serde(with = "groth16::serde::serde_fr")] Fr);

impl SecretKey {
    #[must_use]
    pub const fn zero() -> Self {
        Self(Fr::ZERO)
    }

    #[must_use]
    pub const fn one() -> Self {
        Self(Fr::ONE)
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
    pub fn into_inner(self) -> Fr {
        self.0
    }

    pub fn sign(&self, data: &Fr) -> Result<Signature, ZkSignError> {
        Self::multi_sign(std::slice::from_ref(self), data)
    }

    pub fn multi_sign(keys: &[Self], data: &Fr) -> Result<Signature, ZkSignError> {
        let sk_inputs = try_from_secret_keys(keys)?;
        let inputs = ZkSignWitnessInputs::from_witness_data_and_message_hash(sk_inputs, *data);

        let (signature, _) = zksign::prove(&inputs).expect("Signature should succeed");
        Ok(Signature::new(signature))
    }

    #[must_use]
    pub fn to_public_key(&self) -> PublicKey {
        PublicKey::new(<Poseidon2Bn254Hasher as Digest>::compress(&[*KDF, self.0]))
    }
}

impl PartialEq for SecretKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.0.0.ct_eq(&other.0.0.0).into()
    }
}

impl Eq for SecretKey {}

impl From<BigUint> for SecretKey {
    fn from(value: BigUint) -> Self {
        Self(value.into())
    }
}

fn try_from_secret_keys(keys: &[SecretKey]) -> Result<ZkSignPrivateKeysData, ZkSignError> {
    let len = keys.len();
    if len > 32 {
        return Err(ZkSignError::TooManyKeys(len));
    }
    let mut buff: [Fr; 32] = [Fr::ZERO; 32];
    for (i, sk) in keys.iter().enumerate() {
        buff[i] = sk.clone().into_inner();
    }
    Ok(buff.into())
}
