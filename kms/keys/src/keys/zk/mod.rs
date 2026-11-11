use core::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::Fr;
use lb_zktransfer::{Unsigned, ZkTransferProof, ZkTransferWitnessBuilder};
use num_bigint::BigUint;
use serde::Deserialize;
use zeroize::ZeroizeOnDrop;

use crate::keys::{errors::KeyError, secured_key::SecuredKey};

mod private;
pub use self::private::{PK0, SecretKey as UnsecuredZkKey, kdf_public_key};
mod public;
pub use self::public::PublicKey;

// The input secret keys are set by the KMS.
// The witness of a transfer without the secret keys of its inputs, which the
// keys of the KMS complete.
pub type ZkTransferPayload = ZkTransferWitnessBuilder<Unsigned>;

/// An hardened ZK secret key that only exposes methods to retrieve public
/// information.
///
/// It is a secured variant of a [`UnsecuredZkKey`] and used within the set of
/// supported KMS keys.
#[derive(Deserialize, ZeroizeOnDrop, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "unsafe", derive(serde::Serialize))]
pub struct ZkKey(UnsecuredZkKey);

impl ZkKey {
    #[must_use]
    pub const fn new(secret_key: Fr) -> Self {
        Self(UnsecuredZkKey::new(secret_key))
    }

    #[must_use]
    pub const fn zero() -> Self {
        Self(UnsecuredZkKey::zero())
    }

    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        self.0.as_fr()
    }

    /// The nullifier key of the notes of this spending secret key.
    #[must_use]
    pub fn nullifier_key(&self) -> Fr {
        self.0.nullifier_key()
    }

    #[must_use]
    pub fn to_public_key(&self) -> PublicKey {
        self.0.to_public_key()
    }

    #[cfg(feature = "unsafe")]
    #[must_use]
    pub fn into_unsecured(self) -> UnsecuredZkKey {
        self.0.clone()
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
        write!(f, "ZkKey({:?})", self.0)?;

        #[cfg(not(feature = "unsafe"))]
        write!(f, "ZkKey(<redacted>)")?;

        Ok(())
    }
}

impl From<Fr> for ZkKey {
    fn from(value: Fr) -> Self {
        Self(UnsecuredZkKey::new(value))
    }
}

impl From<BigUint> for ZkKey {
    fn from(value: BigUint) -> Self {
        Self(value.into())
    }
}

impl From<UnsecuredZkKey> for ZkKey {
    fn from(value: UnsecuredZkKey) -> Self {
        Self(value)
    }
}

impl BinaryEncode for PublicKey {
    fn encoded_length(&self) -> usize {
        self.as_fr().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.as_fr().encode_into(out);
    }
}

impl BinaryDecode for PublicKey {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, inner) = Fr::decode(input, &())?;
        Ok((rest, Self::new(inner)))
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
        self.0.prove_transfer_alone(payload.clone())
    }

    // The i-th input is proven with the i-th key.
    fn sign_multiple(
        keys: &[&Self],
        payload: &Self::Payload,
    ) -> Result<Self::Signature, Self::Error> {
        UnsecuredZkKey::prove_transfer(
            &keys.iter().map(|key| key.0.clone()).collect::<Vec<_>>(),
            payload.clone(),
        )
    }

    fn as_public_key(&self) -> Self::PublicKey {
        self.0.to_public_key()
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
}
