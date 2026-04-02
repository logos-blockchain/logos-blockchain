use std::{fs::read_to_string, path::PathBuf};

const LOGOS_BLOCKCHAIN_CIRCUITS_ENV_VAR: &str = "LOGOS_BLOCKCHAIN_CIRCUITS";
const LOGOS_BLOCKCHAIN_CIRCUITS_DEFAULT_DIR: &str = ".logos-blockchain-circuits";
const EXPECTED_CIRCUITS_VERSION: &str = "v0.4.2";

#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "witness_generator.exe";
#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "witness_generator";

/// Get the logos-blockchain-circuits base directory.
///
/// This function checks the `LOGOS_BLOCKCHAIN_CIRCUITS` environment variable
/// first, and falls back to `~/.logos-blockchain-circuits/` if not set.
///
/// # Panics
///
/// Panics if a logos-blockchain-circuits directory is not found or if the
/// circuits version does not match the expected one.
#[must_use]
pub fn circuits_dir() -> PathBuf {
    // Check LOGOS_BLOCKCHAIN_CIRCUITS env var first
    if let Ok(path_str) = std::env::var(LOGOS_BLOCKCHAIN_CIRCUITS_ENV_VAR) {
        let path = PathBuf::from(path_str);
        if path.is_dir() {
            return path;
        }
        panic!(
            "{LOGOS_BLOCKCHAIN_CIRCUITS_ENV_VAR} environment variable is set to '{}', but this path does not exist or is not a directory",
            path.display()
        )
    }
    // Fall back to ~/.logos-blockchain-circuits/
    let path = dirs::home_dir()
        .expect("user does not have a home directory?")
        .join(LOGOS_BLOCKCHAIN_CIRCUITS_DEFAULT_DIR);

    if path.is_dir() {
        let circuits_version = read_to_string(path.join("VERSION"))
            .unwrap()
            .trim()
            .to_owned();
        assert_eq!(
            circuits_version,
            EXPECTED_CIRCUITS_VERSION,
            "The logos-blockchain-circuits directory at '{}' is version '{circuits_version}', but version '{EXPECTED_CIRCUITS_VERSION}' is expected. Please update your circuits directory or set the {LOGOS_BLOCKCHAIN_CIRCUITS_ENV_VAR} environment variable to point to a compatible circuits directory.",
            path.display()
        );
        path
    } else {
        panic!(
            "Could not find logos-blockchain-circuits directory. Please either:\n\
             1. Set the {LOGOS_BLOCKCHAIN_CIRCUITS_ENV_VAR} environment variable to point to your logos-blockchain-circuits directory, or\n\
             2. Place the logos-blockchain-circuits release at {}\n",
            path.display()
        )
    }
}

/// Path to a witness generator binary for a specific circuit.
///
/// # Arguments
///
/// * `circuit_name` - The name of the circuit (e.g., "zksign")
///
/// # Panics
///
/// Panics if the witness generator binary is not found at the expected path.
#[must_use]
pub fn witness_generator_path(circuit_name: &str) -> PathBuf {
    let base_dir = circuits_dir();
    let witness_gen_path = base_dir.join(circuit_name).join(BINARY_NAME);

    if witness_gen_path.is_file() {
        witness_gen_path
    } else {
        panic!(
            "Witness generator not found at expected path: {}\n\
             Please ensure your logos-blockchain-circuits directory has the correct structure for circuit '{circuit_name}'",
            witness_gen_path.display()
        )
    }
}

/// Path to a proving key for a specific circuit.
///
/// # Arguments
///
/// * `circuit_name` - The name of the circuit (e.g., "zksign")
///
/// # Panics
///
/// Panics if the proving key (.zkey file) is not found at the expected path.
#[must_use]
pub fn proving_key_path(circuit_name: &str) -> PathBuf {
    let base_dir = circuits_dir();
    let proving_key_path = base_dir.join(circuit_name).join("proving_key.zkey");

    if proving_key_path.is_file() {
        proving_key_path
    } else {
        panic!(
            "Proving key not found at expected path: {}\n\
             Please ensure your logos-blockchain-circuits directory has the correct structure for circuit '{circuit_name}'",
            proving_key_path.display()
        )
    }
}

/// Path to a verification key for a specific circuit.
///
/// # Arguments
///
/// * `circuit_name` - The name of the circuit (e.g., "zksign")
///
/// # Panics
///
/// Panics if the verification key JSON file is not found at the expected path.
#[must_use]
pub fn verification_key_path(circuit_name: &str) -> PathBuf {
    let base_dir = circuits_dir();
    let verification_key_path = base_dir.join(circuit_name).join("verification_key.json");

    if verification_key_path.is_file() {
        verification_key_path
    } else {
        panic!(
            "Verification key not found at expected path: {}\n\
             Please ensure your logos-blockchain-circuits directory has the correct structure for circuit '{circuit_name}'",
            verification_key_path.display()
        )
    }
}
