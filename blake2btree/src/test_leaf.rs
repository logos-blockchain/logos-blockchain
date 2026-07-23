use rand::RngCore;

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

impl AsRef<[u8; 32]> for TestLeaf {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}
