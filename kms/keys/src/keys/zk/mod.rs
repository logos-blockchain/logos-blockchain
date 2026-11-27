use core::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};

use lb_groth16::Fr;
use lb_zktransfer::{Unsigned, ZkTransferProof, ZkTransferWitnessBuilder};
use num_bigint::BigUint;
use serde::{Deserialize, Deserializer};
use zeroize::{ZeroizeOnDrop, Zeroizing};

use crate::keys::{errors::KeyError, secured_key::SecuredKey};

mod private;
pub use self::private::{PK0, SecretKey as UnsecuredZkKey, kdf_public_key};
mod public;
pub use self::public::PublicKey;

// The input secret keys are set by the KMS.
// The witness of a transfer without the secret keys of its inputs, which the
// keys of the KMS complete.
pub type ZkTransferPayload = ZkTransferWitnessBuilder<Unsigned>;

/// A secured ZK credential used by the KMS.
///
/// In-process operators are trusted with its secret material; this type does
/// not establish confidentiality against arbitrary operator implementations.
#[derive(ZeroizeOnDrop, Clone, PartialEq, Eq)]
pub struct ZkKey(ZkKeyMaterial);

/// The untagged spending variant preserves the existing serialized format.
/// A directly supplied credential has an explicit, distinct representation.
#[derive(Deserialize, ZeroizeOnDrop, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "unsafe", derive(serde::Serialize))]
#[serde(untagged)]
enum ZkKeyMaterial {
    Spending(UnsecuredZkKey),
    Direct { direct: UnsecuredZkKey },
}

// Binary spending keys keep their previous fixed-width representation. The
// field modulus leaves the high bit clear, so it distinguishes direct keys
// without changing the old spending encoding.
impl<'de> Deserialize<'de> for ZkKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            return ZkKeyMaterial::deserialize(deserializer).map(Self);
        }
        let mut bytes = Zeroizing::new(lb_utils::serde::deserialize_bytes_array::<32, _>(
            deserializer,
        )?);
        let direct = bytes[31] & 0x80 != 0;
        bytes[31] &= 0x7f;
        let secret = lb_groth16::fr_from_bytes(bytes.as_ref())
            .map_err(|_| serde::de::Error::custom("invalid ZK credential"))?;
        Ok(if direct {
            Self::from_direct_secret(secret)
        } else {
            Self::new(secret)
        })
    }
}

#[cfg(feature = "unsafe")]
impl serde::Serialize for ZkKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            return serde::Serialize::serialize(&self.0, serializer);
        }
        let mut bytes = Zeroizing::new(lb_groth16::fr_to_bytes(self.as_fr()));
        if !self.is_spending_key() {
            bytes[31] |= 0x80;
        }
        lb_utils::serde::serialize_bytes_array(*bytes, serializer)
    }
}

impl ZkKey {
    #[must_use]
    pub const fn new(secret_key: Fr) -> Self {
        Self(ZkKeyMaterial::Spending(UnsecuredZkKey::new(secret_key)))
    }

    #[must_use]
    pub const fn zero() -> Self {
        Self(ZkKeyMaterial::Spending(UnsecuredZkKey::zero()))
    }

    /// Import a secret used directly for its public-key derivation. It is not
    /// a spending credential and cannot sign transactions.
    #[must_use]
    pub const fn from_direct_secret(secret: Fr) -> Self {
        Self(ZkKeyMaterial::Direct {
            direct: UnsecuredZkKey::new(secret),
        })
    }

    #[must_use]
    pub const fn is_spending_key(&self) -> bool {
        matches!(self.0, ZkKeyMaterial::Spending(_))
    }

    /// Trusted in-process KMS operators can access the underlying material.
    /// This interface does not protect secrets from an operator implementation.
    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        match &self.0 {
            ZkKeyMaterial::Spending(key) | ZkKeyMaterial::Direct { direct: key } => key.as_fr(),
        }
    }

    /// The derived nullifier key for spending credentials, or the directly
    /// supplied material for a direct credential. Operators are trusted with
    /// this material, as with [`Self::as_fr`].
    #[must_use]
    pub fn nullifier_key(&self) -> Fr {
        match &self.0 {
            ZkKeyMaterial::Spending(key) => key.nullifier_key(),
            ZkKeyMaterial::Direct { direct } => *direct.as_fr(),
        }
    }

    #[must_use]
    pub fn to_public_key(&self) -> PublicKey {
        kdf_public_key(&self.nullifier_key())
    }

    #[cfg(feature = "unsafe")]
    #[must_use]
    pub fn into_unsecured(self) -> Option<UnsecuredZkKey> {
        match &self.0 {
            ZkKeyMaterial::Spending(key) => Some(key.clone()),
            ZkKeyMaterial::Direct { .. } => None,
        }
    }
}

impl Hash for ZkKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.to_public_key().hash(state);
    }
}

impl Debug for ZkKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        #[cfg(feature = "unsafe")]
        match &self.0 {
            ZkKeyMaterial::Spending(key) => write!(f, "ZkKey({key:?})")?,
            ZkKeyMaterial::Direct { .. } => write!(f, "ZkKey(Direct(<redacted>))")?,
        }

        #[cfg(not(feature = "unsafe"))]
        write!(f, "ZkKey(<redacted>)")?;

        Ok(())
    }
}

impl From<Fr> for ZkKey {
    fn from(value: Fr) -> Self {
        Self::new(value)
    }
}

impl From<BigUint> for ZkKey {
    fn from(value: BigUint) -> Self {
        Self(ZkKeyMaterial::Spending(value.into()))
    }
}

impl From<UnsecuredZkKey> for ZkKey {
    fn from(value: UnsecuredZkKey) -> Self {
        Self(ZkKeyMaterial::Spending(value))
    }
}

#[async_trait::async_trait]
impl SecuredKey for ZkKey {
    type Payload = ZkTransferPayload;
    type Signature = ZkTransferProof;
    type PublicKey = PublicKey;
    type Error = KeyError;

    // Every input is proven with this key.
    fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error> {
        match &self.0 {
            ZkKeyMaterial::Spending(key) => key.prove_transfer_alone(payload.clone()),
            ZkKeyMaterial::Direct { .. } => Err(KeyError::UnsupportedKey(
                "Direct ZK credentials cannot sign transfers".into(),
            )),
        }
    }

    // The i-th input is proven with the i-th key.
    fn sign_multiple(
        keys: &[&Self],
        payload: &Self::Payload,
    ) -> Result<Self::Signature, Self::Error> {
        let spending_keys = keys
            .iter()
            .map(|key| match &key.0 {
                ZkKeyMaterial::Spending(key) => Ok(key.clone()),
                ZkKeyMaterial::Direct { .. } => Err(KeyError::UnsupportedKey(
                    "Direct ZK credentials cannot sign transfers".into(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        UnsecuredZkKey::prove_transfer(&spending_keys, payload.clone())
    }

    fn as_public_key(&self) -> Self::PublicKey {
        self.to_public_key()
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
    use lb_zktransfer::{
        prove,
        test_utils::{sample_unsigned_witness, sample_witness_data},
        verify,
    };
    use num_bigint::BigUint;

    use super::{PK0, ZkKey, kdf_public_key};
    use crate::keys::{errors::KeyError, secured_key::SecuredKey as _};

    #[test]
    fn the_public_key_derives_from_the_nullifier_key_the_default_staking_secret_key() {
        let key = ZkKey::new(Fr::from(7u64));
        // NF_KDF_V1 as a little-endian integer
        let nf_kdf = Fr::from(910_114_230_080_953_599_566u128);
        assert_eq!(
            key.nullifier_key(),
            Poseidon2Bn254Hasher::compress(&[nf_kdf, Fr::from(7u64)])
        );
        // A wallet stakes with the nullifier key, so its notes have one key.
        assert_eq!(kdf_public_key(&key.nullifier_key()), key.to_public_key());
        assert_ne!(kdf_public_key(key.as_fr()), key.to_public_key());
    }

    #[test]
    fn program_notes_are_held_by_the_key_of_the_spending_secret_key_0() {
        // PK0 as the circuits fix it
        let pk0 = BigUint::parse_bytes(
            b"972202553888197888567329063379813321440011391652189983173398887118271398762",
            10,
        )
        .unwrap();
        assert_eq!(*PK0.as_fr(), Fr::from(pk0));
        assert_eq!(*PK0, ZkKey::new(Fr::ZERO).to_public_key());
    }

    #[test]
    fn sign_multiple_proves_the_transfer_with_one_key_per_active_input() {
        let (_, public) = prove(sample_witness_data().into()).unwrap();
        let (payload, secret_keys) = sample_unsigned_witness();
        let keys: Vec<ZkKey> = secret_keys.into_iter().map(ZkKey::new).collect();
        assert_eq!(keys.len(), 2);

        let proof = ZkKey::sign_multiple(&[&keys[0], &keys[1]], &payload).unwrap();
        assert!(verify(&proof, &public).unwrap());
        assert!(matches!(
            ZkKey::sign_multiple(&[&keys[0]], &payload),
            Err(KeyError::UnsupportedMultisignatureSize(2, 1))
        ));
    }

    #[test]
    fn direct_credentials_use_the_secret_without_a_second_derivation_and_cannot_sign() {
        let secret = Fr::from(7u64);
        let direct = ZkKey::from_direct_secret(secret);
        let spending = ZkKey::new(secret);
        assert_eq!(direct.to_public_key(), kdf_public_key(&secret));
        assert_eq!(direct.nullifier_key(), secret);
        assert_ne!(direct.to_public_key(), spending.to_public_key());
        let (payload, _) = sample_unsigned_witness();
        assert!(matches!(
            direct.sign(&payload),
            Err(KeyError::UnsupportedKey(_))
        ));
        assert!(matches!(
            ZkKey::sign_multiple(&[&spending, &direct], &payload),
            Err(KeyError::UnsupportedKey(_))
        ));
        assert!(matches!(
            ZkKey::sign_multiple(&[&direct, &spending], &payload),
            Err(KeyError::UnsupportedKey(_))
        ));
        assert!(!format!("{direct:?}").contains(&format!("{secret:?}")));
        #[cfg(feature = "unsafe")]
        assert!(direct.into_unsecured().is_none());
    }

    #[test]
    fn legacy_spending_and_explicit_direct_credentials_deserialize() {
        let secret = Fr::from(42u64);
        let legacy = serde_json::to_value(super::UnsecuredZkKey::new(secret)).unwrap();
        let spending: ZkKey = serde_json::from_value(legacy.clone()).unwrap();
        let direct: ZkKey = serde_json::from_value(serde_json::json!({"direct": legacy})).unwrap();
        assert_eq!(spending, ZkKey::new(secret));
        assert_eq!(direct, ZkKey::from_direct_secret(secret));
        #[cfg(feature = "unsafe")]
        {
            let spending_json = serde_json::to_value(&spending).unwrap();
            let direct_json = serde_json::to_value(&direct).unwrap();
            assert_eq!(
                serde_json::from_value::<ZkKey>(spending_json).unwrap(),
                spending
            );
            assert_eq!(
                serde_json::from_value::<ZkKey>(direct_json).unwrap(),
                direct
            );
            let spending_binary = bincode::serialize(&spending).unwrap();
            assert_eq!(
                spending_binary,
                bincode::serialize(&super::UnsecuredZkKey::new(secret)).unwrap()
            );
            assert_eq!(
                bincode::deserialize::<ZkKey>(&spending_binary).unwrap(),
                spending
            );
            let direct_binary = bincode::serialize(&direct).unwrap();
            assert_eq!(
                bincode::deserialize::<ZkKey>(&direct_binary).unwrap(),
                direct
            );
        }
    }

    #[test]
    fn binary_and_direct_credentials_reject_noncanonical_field_elements() {
        let mut invalid = [0xff; 32];
        invalid[31] = 0x7f;
        assert!(bincode::deserialize::<ZkKey>(&invalid).is_err());
        invalid[31] |= 0x80;
        assert!(bincode::deserialize::<ZkKey>(&invalid).is_err());
        let direct = serde_json::json!({"direct": hex::encode([0xff; 32])});
        assert!(serde_json::from_value::<ZkKey>(direct).is_err());
    }
}
