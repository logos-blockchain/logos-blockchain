use std::sync::LazyLock;

use lb_groth16::{AdditiveGroup as _, Field as _, Fr, fr_from_bytes_unchecked, fr_from_mod_bytes};
use lb_poseidon2::{Digest, Poseidon2Bn254Hasher};
use lb_zktransfer::{
    ZkTransferProof, ZkTransferSecretKeysError, ZkTransferWitnessInputsData,
    prove as prove_zk_transfer,
};
use num_bigint::BigUint;
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use zeroize::ZeroizeOnDrop;

use crate::keys::{
    errors::KeyError,
    zk::{ZkTransferPayload, public::PublicKey},
};

fn prove(witness: ZkTransferWitnessInputsData) -> Result<ZkTransferProof, KeyError> {
    prove_zk_transfer(witness.into())
        .map(|(proof, _)| proof)
        .map_err(|e| KeyError::ZkTransferProve(e.to_string()))
}

static KDF: LazyLock<Fr> = LazyLock::new(|| fr_from_bytes_unchecked(b"KDF"));
static NF_KDF_V1: LazyLock<Fr> = LazyLock::new(|| fr_from_bytes_unchecked(b"NF_KDF_V1"));

/// A ZK secret key exposing methods to retrieve its inner secret value.
///
/// It is the spending secret key of the notes under its public key: it derives
/// their nullifier key, which derives their public key and their nullifiers.
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

    // keys[i] proves the i-th input
    pub fn prove_transfer(
        keys: &[Self],
        payload: ZkTransferPayload,
    ) -> Result<ZkTransferProof, KeyError> {
        let secret_keys: Vec<Fr> = keys.iter().map(|key| key.0).collect();
        let witness = payload
            .with_secret_keys(&secret_keys)
            .map_err(|ZkTransferSecretKeysError::KeyCount { inputs, keys }| {
                KeyError::UnsupportedMultisignatureSize(inputs, keys)
            })?
            .build();
        prove(witness)
    }

    // This key proves every input, the notes of a single holder.
    pub fn prove_transfer_alone(
        &self,
        payload: ZkTransferPayload,
    ) -> Result<ZkTransferProof, KeyError> {
        prove(payload.with_secret_key(self.0).build())
    }

    /// The nullifier key `nf_sk = zkhash(NF_KDF_V1, spending_sk)`, which
    /// derives the public key and the nullifiers of the notes. A wallet
    /// uses it as the staking secret key of its notes, so a node handed it
    /// leads with them without being able to spend them.
    #[must_use]
    pub fn nullifier_key(&self) -> Fr {
        <Poseidon2Bn254Hasher as Digest>::compress(&[*NF_KDF_V1, self.0])
    }

    /// The public key `zkhash(KDF, nf_sk)`, derived from the nullifier key.
    #[must_use]
    pub fn to_public_key(&self) -> PublicKey {
        kdf_public_key(&self.nullifier_key())
    }

    #[must_use]
    pub fn from_rng<Rng: RngCore + CryptoRng>(rng: &mut Rng) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(fr_from_mod_bytes(&bytes))
    }
}

/// The key `zkhash(KDF, secret)` of a secret.
///
/// It is the public key of a nullifier key, the staking key of a staking
/// secret key and the `zk_id` of a quota secret key. With the nullifier key as
/// staking secret key, the default of a wallet, the staking key of a note is
/// its public key.
#[must_use]
pub fn kdf_public_key(secret: &Fr) -> PublicKey {
    PublicKey::new(<Poseidon2Bn254Hasher as Digest>::compress(&[*KDF, *secret]))
}

/// `PK0`, the public key of the spending secret key `0`, which holds every
/// program note: everyone knows its nullifier key `NF_SK0`, so only the proof
/// of its program moves a program note.
pub static PK0: LazyLock<PublicKey> = LazyLock::new(|| SecretKey(Fr::ZERO).to_public_key());

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
