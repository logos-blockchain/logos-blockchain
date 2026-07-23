use rand::RngCore;

use crate::{Hash, LeafHash};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TestLeaf([u8; 32]);

impl TestLeaf {
    pub fn from_rng<Rng: RngCore>(rng: &mut Rng) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    #[must_use]
    pub fn from_usize(n: usize) -> Self {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
        Self(bytes)
    }
}

// The element is already a 32 bytes value, so it is used as the leaf as is.
impl LeafHash<Self> for TestLeaf {
    fn leaf_hash(&self, _key: &Self) -> Hash {
        self.0
    }
}