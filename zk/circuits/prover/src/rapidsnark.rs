use std::{
    io::{Error, Result, Write as _},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use lb_circuits_utils::circuits_dir;
use tempfile::NamedTempFile;

#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "prover.exe";
#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "prover";

/// Path to the prover binary in the `LOGOS_BLOCKCHAIN_CIRCUITS` directory.
///
/// # Panics
///
/// Panics if the prover binary is not found at the expected path.
fn prover_binary() -> PathBuf {
    // Get the logos-blockchain-circuits directory
    let circuits_dir = circuits_dir();

    // Check for prover binary at the root of logos-blockchain-circuits directory
    let prover_path = circuits_dir.join(BINARY_NAME);
    if prover_path.is_file() {
        return prover_path;
    }

    panic!(
        "Could not find '{BINARY_NAME}' binary at expected path: {}\n\
         Please ensure your logos-blockchain-circuits directory has the correct structure with the prover binary at the root.",
        prover_path.display()
    )
}

static BINARY: LazyLock<PathBuf> = LazyLock::new(prover_binary);

/// Runs the `prover` command to generate a proof and public inputs for the
/// given circuit and witness contents.
///
/// # Arguments
///
/// * `circuit_file` - The path to the file containing the circuit (proving
///   key).
/// * `witness_file` - The path to the file containing the witness.
/// * `proof_file` - The path to the file where the proof will be written.
/// * `public_file` - The path to the file where the public inputs will be
///   written.
///
/// # Returns
///
/// A [`Result`] which contains the paths to the proof file and public inputs
/// file if successful.
pub fn prover(
    proving_key: &Path,
    witness_file: &Path,
    proof_file: &Path,
    public_file: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let output = std::process::Command::new(BINARY.to_owned())
        .arg(proving_key)
        .arg(witness_file)
        .arg(proof_file)
        .arg(public_file)
        .output()?;

    if !output.status.success() {
        let error_message = String::from_utf8_lossy(&output.stderr);
        return Err(Error::other(format!(
            "prover command failed: {error_message}"
        )));
    }

    Ok((proof_file.to_owned(), public_file.to_owned()))
}

/// Runs the `prover` command to generate a proof and public inputs for the
/// given circuit and witness contents.
///
/// # Note
///
/// Calls [`prover`] underneath but hides the file handling details.
///
/// # Arguments
///
/// * `circuit_contents` - A byte slice containing the circuit (proving key).
/// * `witness_contents` - A byte slice containing the witness.
///
/// # Returns
///
/// A [`Result`] which contains the proof and public inputs as strings if
/// successful.
pub fn prover_from_contents(
    proving_key_path: &Path,
    witness_contents: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut witness_file = NamedTempFile::new()?;
    let proof_file = NamedTempFile::new()?;
    let public_file = NamedTempFile::new()?;
    witness_file.write_all(witness_contents)?;

    prover(
        proving_key_path,
        witness_file.path(),
        proof_file.path(),
        public_file.path(),
    )?;

    let mut proof = std::fs::read(proof_file)?;
    // binary file return a proper json with trailing null bytes
    // remove trailing null bytes
    proof.retain(|byte| *byte != b'\0');
    let mut public = std::fs::read(public_file)?;
    // remove trailing null bytes
    public.retain(|byte| *byte != b'\0');
    Ok((proof, public))
}

#[cfg(test)]
mod tests {
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
    fn test_prover() {
        let circuit_file = CIRCUIT_ZKEY.clone();
        let witness_file = WITNESS_WTNS.clone();
        let proof_file = NamedTempFile::new().unwrap();
        let public_file = NamedTempFile::new().unwrap();

        let result = prover(
            &circuit_file,
            &witness_file,
            proof_file.path(),
            public_file.path(),
        )
        .unwrap();
        assert_eq!(
            result.0,
            proof_file.path(),
            "The proof file path should match the expected path"
        );
        assert_eq!(
            result.1,
            public_file.path(),
            "The public file path should match the expected path"
        );

        let proof_content = std::fs::read_to_string(proof_file.path()).unwrap();
        assert!(
            !proof_content.is_empty(),
            "The proof file should not be empty"
        );

        let public_content = std::fs::read_to_string(public_file.path()).unwrap();
        assert!(
            !public_content.is_empty(),
            "The public file should not be empty"
        );
    }

    #[test]
    fn test_prover_invalid_input() {
        let circuit_file = CIRCUIT_ZKEY.clone();
        let mut witness_file = NamedTempFile::new().unwrap();
        witness_file.write_all(b"invalid witness").unwrap();
        let proof_file = NamedTempFile::new().unwrap();
        let public_file = NamedTempFile::new().unwrap();

        let result = prover(
            &circuit_file,
            witness_file.path(),
            proof_file.path(),
            public_file.path(),
        );
        assert!(
            result.is_err(),
            "Expected prover to fail with invalid input"
        );
    }

    #[test]
    fn test_prover_from_contents() {
        let witness_contents = std::fs::read(&*WITNESS_WTNS).unwrap();

        let (proof, public) =
            prover_from_contents(CIRCUIT_ZKEY.as_path(), &witness_contents).unwrap();
        assert!(!proof.is_empty(), "The proof should not be empty");
        assert!(!public.is_empty(), "The public inputs should not be empty");
    }

    #[test]
    fn test_prover_from_contents_invalid() {
        let invalid_witness_contents = b"invalid witness";

        let result = prover_from_contents(&CIRCUIT_ZKEY, invalid_witness_contents);
        assert!(
            result.is_err(),
            "Expected prover_from_contents to fail with invalid input"
        );
    }
}
