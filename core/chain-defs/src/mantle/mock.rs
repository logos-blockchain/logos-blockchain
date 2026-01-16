use std::convert::Infallible;

use blake2::{
    Blake2bVar,
    digest::{Update as _, VariableOutput as _},
};
use groth16::Fr;
use num_bigint::BigUint;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    codec::SerializeOp as _,
    mantle::{Transaction, TransactionHasher, TxHash},
};

#[derive(Debug, Clone, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MockTransaction<M> {
    id: MockTxId,
    content: M,
}

impl<M: Serialize + DeserializeOwned + Clone> MockTransaction<M> {
    pub fn new(content: M) -> Self {
        let id = MockTxId::try_from(
            content
                .to_bytes()
                .expect("MockTransaction should be able to be serialized")
                .as_ref(),
        )
        .unwrap();
        Self { id, content }
    }

    pub const fn message(&self) -> &M {
        &self.content
    }

    pub const fn id(&self) -> MockTxId {
        self.id
    }
}

impl<M: Serialize + DeserializeOwned + Clone> Transaction for MockTransaction<M> {
    const HASHER: TransactionHasher<Self> = Self::id;
    type Hash = MockTxId;

    fn as_signing_frs(&self) -> Vec<Fr> {
        todo!()
    }
}

#[expect(
    clippy::fallible_impl_from,
    reason = "`TryFrom` impl conflicting with blanket impl."
)]
impl<M: Serialize + DeserializeOwned + Clone> From<M> for MockTransaction<M> {
    fn from(msg: M) -> Self {
        let id = MockTxId::try_from(
            msg.to_bytes()
                .expect("MockTransaction should be able to be serialized")
                .as_ref(),
        )
        .unwrap();
        Self { id, content: msg }
    }
}

#[derive(
    Debug, Eq, Hash, PartialEq, Ord, Copy, Clone, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct MockTxId([u8; 32]);

impl From<[u8; 32]> for MockTxId {
    fn from(tx_id: [u8; 32]) -> Self {
        Self(tx_id)
    }
}

impl core::ops::Deref for MockTxId {
    type Target = [u8; 32];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[u8]> for MockTxId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl MockTxId {
    #[must_use]
    pub const fn new(tx_id: [u8; 32]) -> Self {
        Self(tx_id)
    }
}

#[expect(clippy::infallible_try_from, reason = "We want a fallible API.")]
impl TryFrom<&[u8]> for MockTxId {
    type Error = Infallible;

    fn try_from(msg: &[u8]) -> Result<Self, Self::Error> {
        let mut hasher = Blake2bVar::new(32).unwrap();
        hasher.update(msg);
        let mut id = [0u8; 32];
        hasher.finalize_variable(&mut id).unwrap();
        Ok(Self(id))
    }
}

impl<M> From<&MockTransaction<M>> for MockTxId {
    fn from(msg: &MockTransaction<M>) -> Self {
        msg.id
    }
}

impl From<MockTxId> for TxHash {
    fn from(id: MockTxId) -> Self {
        let bytes = id.0;
        let big_uint = BigUint::from_bytes_be(&bytes);
        Self::from(big_uint)
    }
}
