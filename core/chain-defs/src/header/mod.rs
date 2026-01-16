use blake2::Digest as _;
use cryptarchia_engine::Slot;
use groth16::fr_to_bytes;
use key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};
use serde::{Deserialize, Serialize};

pub const BEDROCK_VERSION: u8 = 1;

use crate::{
    codec::SerializeOp as _,
    crypto::Hasher,
    mantle::{Transaction as _, TxHash, genesis_tx::GenesisTx},
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof as _},
    utils::{display_hex_bytes_newtype, serde_bytes_newtype},
};

#[derive(Clone, Debug, Eq, PartialEq, Copy, Hash, PartialOrd, Ord)]
pub struct HeaderId([u8; 32]);

#[derive(Clone, Debug, Eq, PartialEq, Copy, Hash)]
pub struct ContentId([u8; 32]);

#[derive(Clone, Debug, Eq, PartialEq, Copy)]
pub struct Nonce([u8; 32]);

#[derive(Clone, Debug, Eq, PartialEq, Copy, Serialize, Deserialize)]
#[repr(u8)]
pub enum Version {
    Bedrock = BEDROCK_VERSION,
}

impl Version {
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    version: Version,
    parent_block: HeaderId,
    slot: Slot,
    block_root: ContentId,
    proof_of_leadership: Groth16LeaderProof,
}

impl Header {
    #[must_use]
    pub const fn version(&self) -> &Version {
        &self.version
    }

    #[must_use]
    pub const fn parent(&self) -> HeaderId {
        self.parent_block
    }

    fn update_hasher(&self, h: &mut Hasher) {
        h.update(b"BLOCK_ID_V1");
        h.update(self.version.as_byte().to_le_bytes());
        h.update(self.parent_block.0);
        h.update(self.slot.to_le_bytes());
        h.update(self.block_root.0);
        h.update(self.proof_of_leadership.voucher_cm().to_bytes());
        h.update(fr_to_bytes(&self.proof_of_leadership.entropy()));
        h.update(self.proof_of_leadership.proof().to_bytes());
        h.update(self.proof_of_leadership.leader_key().to_bytes());
    }

    #[must_use]
    pub fn id(&self) -> HeaderId {
        let mut h = Hasher::new();
        self.update_hasher(&mut h);
        HeaderId(h.finalize().into())
    }

    #[must_use]
    pub const fn leader_proof(&self) -> &Groth16LeaderProof {
        &self.proof_of_leadership
    }

    #[must_use]
    pub const fn block_root(&self) -> &ContentId {
        &self.block_root
    }

    #[must_use]
    pub const fn slot(&self) -> Slot {
        self.slot
    }

    pub fn sign(&self, signing_key: &Ed25519Key) -> Result<Ed25519Signature, crate::block::Error> {
        let header_bytes = self.to_bytes()?;
        Ok(signing_key.sign_payload(&header_bytes))
    }

    #[must_use]
    pub const fn parent_block(&self) -> HeaderId {
        self.parent_block
    }

    #[must_use]
    pub const fn new(
        parent_block: HeaderId,
        block_root: ContentId,
        slot: Slot,
        proof_of_leadership: Groth16LeaderProof,
    ) -> Self {
        Self {
            version: Version::Bedrock,
            parent_block,
            slot,
            block_root,
            proof_of_leadership,
        }
    }

    #[must_use]
    pub fn genesis(tx: &GenesisTx) -> Self {
        let tx_hash: TxHash = tx.hash();
        Self::new(
            HeaderId([0; 32]),
            ContentId::from(fr_to_bytes(&tx_hash.0)),
            Slot::from(0u64),
            Groth16LeaderProof::genesis(),
        )
    }
}

impl From<[u8; 32]> for HeaderId {
    fn from(id: [u8; 32]) -> Self {
        Self(id)
    }
}

impl From<HeaderId> for [u8; 32] {
    fn from(id: HeaderId) -> Self {
        id.0
    }
}

impl TryFrom<&[u8]> for HeaderId {
    type Error = Error;

    fn try_from(slice: &[u8]) -> Result<Self, Self::Error> {
        if slice.len() != 32 {
            return Err(Error::InvalidHeaderIdSize(slice.len()));
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(slice);
        Ok(Self::from(id))
    }
}

impl AsRef<[u8]> for HeaderId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<[u8; 32]> for ContentId {
    fn from(id: [u8; 32]) -> Self {
        Self(id)
    }
}

impl From<ContentId> for [u8; 32] {
    fn from(id: ContentId) -> Self {
        id.0
    }
}

display_hex_bytes_newtype!(HeaderId);
display_hex_bytes_newtype!(ContentId);

serde_bytes_newtype!(HeaderId, 32);
serde_bytes_newtype!(ContentId, 32);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid header id size: {0}")]
    InvalidHeaderIdSize(usize),
}

#[test]
fn test_serde() {
    use crate::codec::{DeserializeOp as _, SerializeOp as _};
    let header = HeaderId([0; 32]);
    assert_eq!(
        HeaderId::from_bytes(
            &header
                .to_bytes()
                .expect("HeaderId should be able to be serialized")
        )
        .unwrap(),
        HeaderId([0; 32])
    );
}
