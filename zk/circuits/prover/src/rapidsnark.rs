use std::io::Error;

use crate::traits::Prover;

pub struct Rapidsnark;

impl Prover for Rapidsnark {
    type Error = Error;

    fn prove(
        proving_key: &[u8],
        witness_contents: &[u8],
    ) -> Result<rust_rapidsnark::ProofResult, Self::Error> {
        let result =
            rust_rapidsnark::groth16_prover_zkey_buffer_wrapper(proving_key, witness_contents)
                .map_err(|error| Error::other(error.to_string()))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::LazyLock};

    use super::*;

    static PROVING_KEY: LazyLock<Vec<u8>> = LazyLock::new(|| {
        let file = PathBuf::from("../resources/tests/pol/pol.zkey");
        assert!(file.exists(), "Could not find {}.", file.display());
        std::fs::read(&file).expect("Failed to read the proving key file")
    });

    static WITNESS: LazyLock<Vec<u8>> = LazyLock::new(|| {
        let file = PathBuf::from("../resources/tests/pol/witness.wtns");
        assert!(file.exists(), "Could not find {}.", file.display());
        std::fs::read(&file).expect("Failed to read the witness file")
    });

    #[test]
    fn test_prove() {
        let result = Rapidsnark::prove(PROVING_KEY.as_slice(), WITNESS.as_slice()).unwrap();
        assert!(!result.proof.is_empty(), "The proof should not be empty");
        assert!(
            !result.public_signals.is_empty(),
            "The public inputs should not be empty"
        );
    }

    #[test]
    fn test_prove_invalid() {
        let result = Rapidsnark::prove(PROVING_KEY.as_slice(), b"invalid witness");
        assert!(
            result.is_err(),
            "Expected prover to fail with invalid input"
        );
    }
}
