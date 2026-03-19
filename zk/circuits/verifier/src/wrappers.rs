use std::{
    io::{Result, Write as _},
    path::PathBuf,
    sync::LazyLock,
};

use lb_circuits_utils::circuits_dir;
use tempfile::NamedTempFile;

#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "verifier.exe";
#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "verifier";

/// Path to the verifier binary in the `LOGOS_BLOCKCHAIN_CIRCUITS` directory.
///
/// # Panics
///
/// Panics if the verifier binary is not found at the expected path.
fn verifier_binary() -> PathBuf {
    // Get the logos-blockchain-circuits directory
    let circuits_dir = circuits_dir();

    // Check for verifier binary at the root of logos-blockchain-circuits directory
    let verifier_path = circuits_dir.join(BINARY_NAME);
    if verifier_path.is_file() {
        return verifier_path;
    }

    panic!(
        "Could not find '{BINARY_NAME}' binary at expected path: {}\n\
         Please ensure your logos-blockchain-circuits directory has the correct structure with the verifier binary at the root.",
        verifier_path.display()
    )
}

static BINARY: LazyLock<PathBuf> = LazyLock::new(verifier_binary);

/// Runs the `verifier` command to check the validity of a proof for a given
/// verification key and public inputs.
///
/// # Arguments
///
/// * `verification_key_file` - The path to the verification key file.
/// * `public_file` - The path to the public inputs file.
/// * `proof_file` - The path to the proof file.
///
/// # Returns
///
/// A [`Result<bool>`] which indicates whether the verification was
/// successful or not.
fn verifier(
    verification_key_file: &PathBuf,
    public_file: &PathBuf,
    proof_file: &PathBuf,
) -> Result<bool> {
    let output = std::process::Command::new(BINARY.to_owned())
        .arg(verification_key_file)
        .arg(public_file)
        .arg(proof_file)
        .output()?;

    Ok(output.status.success())
}

/// Runs the `verifier` command to check the validity of a proof for a given
/// verification key and public inputs.
///
/// # Note
///
/// Calls [`verifier`] underneath but hides the file handling details.
///
/// # Arguments
///
/// * `verification_key_contents` - A byte slice containing the verification
///   key.
/// * `public_contents` - A byte slice containing the public inputs.
/// * `proof_contents` - A byte slice containing the proof.
///
/// # Returns
///
/// A [`Result<bool>`] which indicates whether the verification was
/// successful or not.
pub fn verifier_from_contents(
    verification_key_contents: &[u8],
    public_contents: &[u8],
    proof_contents: &[u8],
) -> Result<bool> {
    let mut verification_key_file = NamedTempFile::new()?;
    let mut public_file = NamedTempFile::new()?;
    let mut proof_file = NamedTempFile::new()?;
    verification_key_file.write_all(verification_key_contents)?;
    public_file.write_all(public_contents)?;
    proof_file.write_all(proof_contents)?;

    verifier(
        &verification_key_file.path().to_path_buf(),
        &public_file.path().to_path_buf(),
        &proof_file.path().to_path_buf(),
    )
}

#[cfg(test)]
mod tests {
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
    fn test_verifier() {
        let result = verifier(&VERIFICATION_KEY_JSON, &PUBLIC_JSON, &PROOF_JSON);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_verifier_invalid() {
        let mut invalid_proof = NamedTempFile::new().unwrap();
        invalid_proof.write_all(b"invalid proof").unwrap();

        let result = verifier(
            &VERIFICATION_KEY_JSON,
            &PUBLIC_JSON,
            &invalid_proof.path().to_path_buf(),
        );
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_verifier_from_contents() {
        let verification_key_contents = std::fs::read(&*VERIFICATION_KEY_JSON).unwrap();
        let public_contents = std::fs::read(&*PUBLIC_JSON).unwrap();
        let proof_contents = std::fs::read(&*PROOF_JSON).unwrap();

        let result = verifier_from_contents(
            verification_key_contents.as_slice(),
            public_contents.as_slice(),
            proof_contents.as_slice(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_verifier_from_contents_invalid() {
        let verification_key_contents = std::fs::read(&*VERIFICATION_KEY_JSON).unwrap();
        let public_contents = std::fs::read(&*PUBLIC_JSON).unwrap();
        let invalid_proof_contents = b"invalid proof";

        let result = verifier_from_contents(
            verification_key_contents.as_slice(),
            public_contents.as_slice(),
            invalid_proof_contents,
        );
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }
}
