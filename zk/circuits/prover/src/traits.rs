use std::path::Path;

pub use rust_rapidsnark::ProofResult;

pub trait Prover {
    type Error;

    // Takes a path rather than bytes because the underlying rapidsnark C API
    // (`groth16_prover_zkey_file`) requires a file path for the zkey. An
    // in-memory variant (`groth16_prover`) exists in the C library but is not
    // exposed by `rust-rapidsnark` 0.1.3.
    fn prove(proving_key_path: &Path, witness_contents: &[u8]) -> Result<ProofResult, Self::Error>;
}
