use std::sync::LazyLock;

use lb_groth16::{AdditiveGroup as _, Field as _, Fr, fr_from_bytes_unchecked, fr_from_mod_bytes};
use lb_poseidon2::{Digest, Poseidon2Bn254Hasher};
use lb_zktransfer::ZkTransferProof;
use num_bigint::BigUint;
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use zeroize::ZeroizeOnDrop;

use crate::keys::{
    errors::KeyError,
    zk::{ZkTransferPayload, public::PublicKey},
};

static KDF: LazyLock<Fr> = LazyLock::new(|| fr_from_bytes_unchecked(b"KDF"));

/// A ZK secret key exposing methods to retrieve its inner secret value.
///
/// To be used in contexts where a KMS-like key is required, but it's not
/// possible to go through the KMS roundtrip of executing operators.
#[derive(ZeroizeOnDrop, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretKey(#[serde(with = "lb_groth16::serde::serde_fr")] Fr);

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

    // keys[i] proves the i-th active input
    pub fn prove_transfer(
        keys: &[Self],
        mut payload: ZkTransferPayload,
    ) -> Result<ZkTransferProof, KeyError> {
        let active = payload
            .chain
            .inputs_active
            .iter()
            .filter(|active| **active)
            .count();
        if keys.len() != active {
            return Err(KeyError::UnsupportedMultisignatureSize(active, keys.len()));
        }
        let active_inputs = payload
            .wallet
            .inputs
            .iter_mut()
            .zip(payload.chain.inputs_active)
            .filter_map(|(input, active)| active.then_some(input));
        for (input, key) in active_inputs.zip(keys) {
            input.secret_key = key.0;
        }
        lb_zktransfer::prove(payload.into())
            .map(|(proof, _)| proof)
            .map_err(|e| KeyError::ZkTransferProve(e.to_string()))
    }

    #[must_use]
    pub fn to_public_key(&self) -> PublicKey {
        PublicKey::new(<Poseidon2Bn254Hasher as Digest>::compress(&[*KDF, self.0]))
    }

    #[must_use]
    pub fn from_rng<Rng: RngCore + CryptoRng>(rng: &mut Rng) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(fr_from_mod_bytes(&bytes))
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
