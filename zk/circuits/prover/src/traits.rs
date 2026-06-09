pub use rust_rapidsnark::ProofResult;

pub trait Prover {
    type Error;

    fn prove(proving_key: &[u8], witness_contents: &[u8]) -> Result<ProofResult, Self::Error>;
}
