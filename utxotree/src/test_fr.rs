use lb_poseidon2::Fr;
use num_bigint::BigUint;
use rand::RngCore;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct TestFr(Fr);

impl TestFr {
    pub fn from_rng<Rng: RngCore>(rng: &mut Rng) -> Self {
        Self(BigUint::from(rng.next_u64()).into())
    }

    #[must_use]
    pub fn from_usize(n: usize) -> Self {
        Self(BigUint::from(n).into())
    }
}
impl AsRef<Fr> for TestFr {
    fn as_ref(&self) -> &Fr {
        &self.0
    }
}
