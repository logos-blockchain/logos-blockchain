use std::{io::Error, path::Path};

use crate::traits::Prover;

pub struct Rapidsnark;

impl Prover for Rapidsnark {
    type Error = Error;

    fn prove(
        proving_key_path: &Path,
        witness_contents: &[u8],
    ) -> Result<rust_rapidsnark::ProofResult, Self::Error> {
        let zkey_path = proving_key_path
            .to_str()
            .ok_or_else(|| Error::other("invalid UTF-8 in proving key path"))?;
        let result =
            rust_rapidsnark::groth16_prover_zkey_file_wrapper(zkey_path, witness_contents.to_vec())
                .map_err(|error| Error::other(error.to_string()))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::LazyLock};

    use super::*;

    static CIRCUIT_ZKEY: LazyLock<PathBuf> = LazyLock::new(|| {
        let file = PathBuf::from("../resources/tests/pol/pol.zkey");
        assert!(file.exists(), "Could not find {}.", file.display());
        file
    });

    static WITNESS_WTNS: LazyLock<PathBuf> = LazyLock::new(|| {
        let file = PathBuf::from("../resources/tests/pol/witness.wtns");
        assert!(file.exists(), "Could not find {}.", file.display());
        file
    });

    #[test]
    fn test_prove() {
        let witness_contents = std::fs::read(&*WITNESS_WTNS).unwrap();
        let result = Rapidsnark::prove(CIRCUIT_ZKEY.as_path(), &witness_contents).unwrap();
        assert!(!result.proof.is_empty(), "The proof should not be empty");
        assert!(
            !result.public_signals.is_empty(),
            "The public inputs should not be empty"
        );
    }

    #[test]
    fn test_prove_invalid() {
        let result = Rapidsnark::prove(&CIRCUIT_ZKEY, b"invalid witness");
        assert!(
            result.is_err(),
            "Expected prover to fail with invalid input"
        );
    }
}
