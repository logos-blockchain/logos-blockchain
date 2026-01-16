use core::fmt::{self, Debug, Formatter};

use groth16::Fr;
use num_bigint::BigUint;
use serde::Deserialize;
use zeroize::ZeroizeOnDrop;
use zksign::ZkSignError;

use crate::keys::{errors::KeyError, secured_key::SecuredKey};

mod private;
pub use self::private::SecretKey as UnsecuredZkKey;
mod public;
pub use self::public::PublicKey;
mod signature;
pub use self::signature::Signature;

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

    pub(crate) const fn as_fr(&self) -> &Fr {
        self.0.as_fr()
    }

    pub fn sign_payload(&self, data: &Fr) -> Result<Signature, ZkSignError> {
        self.0.sign(data)
    }

    pub fn multi_sign(keys: &[Self], data: &Fr) -> Result<Signature, ZkSignError> {
        UnsecuredZkKey::multi_sign(
            &keys.iter().map(|key| key.0.clone()).collect::<Vec<_>>(),
            data,
        )
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

#[cfg(feature = "unsafe")]
impl From<UnsecuredZkKey> for ZkKey {
    fn from(value: UnsecuredZkKey) -> Self {
        Self(value)
    }
}

#[async_trait::async_trait]
impl SecuredKey for ZkKey {
    type Payload = Fr;
    type Signature = Signature;
    type PublicKey = PublicKey;
    type Error = KeyError;

    fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error> {
        Ok(self.0.sign(payload)?)
    }

    fn sign_multiple(
        keys: &[&Self],
        payload: &Self::Payload,
    ) -> Result<Self::Signature, Self::Error> {
        Ok(UnsecuredZkKey::multi_sign(
            &keys.iter().map(|key| key.0.clone()).collect::<Vec<_>>(),
            payload,
        )?)
    }

    fn as_public_key(&self) -> Self::PublicKey {
        self.0.to_public_key()
    }
}
