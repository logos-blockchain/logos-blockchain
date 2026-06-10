use std::io::Error;

use crate::traits::Verifier;

pub struct Rapidsnark;

impl Verifier for Rapidsnark {
    type Error = Error;

    fn verify(
        verification_key_contents: &[u8],
        public_inputs_contents: &[u8],
        proof_contents: &[u8],
    ) -> Result<bool, Self::Error> {
        let verification_key =
            std::str::from_utf8(verification_key_contents).map_err(Error::other)?;
        let public_inputs = std::str::from_utf8(public_inputs_contents).map_err(Error::other)?;
        let proof = std::str::from_utf8(proof_contents).map_err(Error::other)?;
        rust_rapidsnark::groth16_verify_wrapper(proof, public_inputs, verification_key)
            .map_err(|error| Error::other(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::LazyLock};

    use super::*;

    static VERIFICATION_KEY_JSON: LazyLock<PathBuf> = LazyLock::new(|| {
        let file = PathBuf::from("../resources/tests/pol/verification_key.json");
        assert!(file.exists(), "Could not find {}.", file.display());
        file
    });

    static PROOF_JSON: LazyLock<PathBuf> = LazyLock::new(|| {
        let file = PathBuf::from("../resources/tests/pol/proof.json");
        assert!(file.exists(), "Could not find {}.", file.display());
        file
    });

    static PUBLIC_JSON: LazyLock<PathBuf> = LazyLock::new(|| {
        let file = PathBuf::from("../resources/tests/pol/public.json");
        assert!(file.exists(), "Could not find {}.", file.display());
        file
    });

    #[test]
    fn test_verify() {
        let vk = std::fs::read(&*VERIFICATION_KEY_JSON).unwrap();
        let public = std::fs::read(&*PUBLIC_JSON).unwrap();
        let proof = std::fs::read(&*PROOF_JSON).unwrap();

        let result = Rapidsnark::verify(&vk, &public, &proof);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_verify_invalid() {
        let vk = std::fs::read(&*VERIFICATION_KEY_JSON).unwrap();
        let public = std::fs::read(&*PUBLIC_JSON).unwrap();

        let result = Rapidsnark::verify(&vk, &public, b"invalid proof");
        assert!(!result.is_ok_and(|v| v));
    }
}
