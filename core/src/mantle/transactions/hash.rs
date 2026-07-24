use ark_ff::PrimeField as _;
use bytes::Bytes;
use lb_groth16::Fr;

use crate::{crypto::Hash, utils::serde_bytes_newtype};

/// The hash of a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub struct TxHash(pub Hash);
serde_bytes_newtype!(TxHash, 32);

impl From<Hash> for TxHash {
    fn from(hash: Hash) -> Self {
        Self(hash)
    }
}

impl From<TxHash> for Hash {
    fn from(hash: TxHash) -> Self {
        hash.0
    }
}

impl AsRef<Hash> for TxHash {
    fn as_ref(&self) -> &Hash {
        &self.0
    }
}

impl From<TxHash> for Bytes {
    fn from(tx_hash: TxHash) -> Self {
        Self::copy_from_slice(&tx_hash.0)
    }
}

impl TxHash {
    /// For testing purposes
    #[cfg(test)]
    pub fn random(mut rng: impl rand::RngCore) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    #[must_use]
    pub fn as_signing_bytes(&self) -> Bytes {
        Bytes::from(self.0.to_vec())
    }

    #[must_use]
    pub fn to_fr(&self) -> Fr {
        Fr::from_le_bytes_mod_order(&self.0)
    }
}
