use core::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::Fr;
use num_bigint::BigUint;
use serde::Deserialize;
use zeroize::ZeroizeOnDrop;

use crate::keys::{errors::KeyError, secured_key::SecuredKey};

mod private;
pub use self::private::SecretKey as UnsecuredZkKey;
mod public;
pub use self::public::PublicKey;

// The input secret keys are set by the KMS.
pub type ZkTransferPayload = lb_zktransfer::ZkTransferWitnessInputsData;

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
    type Signature = lb_zktransfer::ZkTransferProof;
    type PublicKey = PublicKey;
    type Error = KeyError;

    // Every active input is proven with this key.
    fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error> {
        let active = payload
            .chain
            .inputs_active
            .iter()
            .filter(|active| **active)
            .count();
        UnsecuredZkKey::prove_transfer(&vec![self.0.clone(); active], payload.clone())
    }

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

    use super::ZkKey;
    use crate::keys::{errors::KeyError, secured_key::SecuredKey as _};

    #[test]
    fn sign_multiple_proves_the_transfer_with_one_key_per_active_input() {
        let sample = lb_zktransfer::test_utils::sample_witness_data();
        let (_, public) = lb_zktransfer::prove(sample.clone().into()).unwrap();
        let keys: Vec<ZkKey> = sample.wallet.inputs[..2]
            .iter()
            .map(|input| ZkKey::new(input.secret_key))
            .collect();
        let mut payload = sample;
        for input in &mut payload.wallet.inputs {
            input.secret_key = Fr::ZERO;
        }

        let proof = ZkKey::sign_multiple(&[&keys[0], &keys[1]], &payload).unwrap();
        assert!(lb_zktransfer::verify(&proof, &public).unwrap());
        assert!(matches!(
            ZkKey::sign_multiple(&[&keys[0]], &payload),
            Err(KeyError::UnsupportedMultisignatureSize(2, 1))
        ));
    }
}
