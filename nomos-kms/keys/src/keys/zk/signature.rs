use generic_array::{
    GenericArray,
    typenum::{U32, U64},
};
use serde::{Deserialize, Serialize};
use zksign::ZkSignProof;

#[derive(Serialize, Deserialize)]
#[serde(remote = "zksign::ZkSignProof")]
struct SignatureSerde {
    pi_a: GenericArray<u8, U32>,
    pi_b: GenericArray<u8, U64>,
    pi_c: GenericArray<u8, U32>,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Signature(#[serde(with = "SignatureSerde")] ZkSignProof);

impl Signature {
    #[must_use]
    pub const fn new(proof: ZkSignProof) -> Self {
        Self(proof)
    }

    #[must_use]
    pub const fn as_proof(&self) -> &ZkSignProof {
        &self.0
    }
}
