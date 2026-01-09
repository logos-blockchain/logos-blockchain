use std::path::PathBuf;

const NOMOS_CIRCUITS_ENV_VAR: &str = "NOMOS_CIRCUITS";
const NOMOS_CIRCUITS_DEFAULT_DIR: &str = ".nomos-circuits";

/// Get the nomos-circuits base directory.
///
/// This function checks the `NOMOS_CIRCUITS` environment variable first,
/// and falls back to `~/.nomos-circuits/` if not set.
///
/// # Panics
///
/// Panics if a nomos-circuits directory is not found
#[must_use]
pub fn nomos_circuits_dir() -> PathBuf {
    // Check NOMOS_CIRCUITS env var first
    if let Ok(path_str) = std::env::var(NOMOS_CIRCUITS_ENV_VAR) {
        let path = PathBuf::from(path_str);
        if path.is_dir() {
            return path;
        }
        panic!(
            "{NOMOS_CIRCUITS_ENV_VAR} environment variable is set to '{}', but this path does not exist or is not a directory",
            path.display()
        )
    }
    // Fall back to ~/.nomos-circuits/
    let path = dirs::home_dir()
        .expect("user does not have a home directory?")
        .join(NOMOS_CIRCUITS_DEFAULT_DIR);

    if path.is_dir() {
        path
    } else {
        panic!(
            "Could not find nomos-circuits directory. Please either:\n\
             1. Set the {NOMOS_CIRCUITS_ENV_VAR} environment variable to point to your nomos-circuits directory, or\n\
             2. Place the nomos-circuits release at {}\n",
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
    let base_dir = nomos_circuits_dir();
    let witness_gen_path = base_dir.join(circuit_name).join("witness_generator");

    if witness_gen_path.is_file() {
        witness_gen_path
    } else {
        panic!(
            "Witness generator not found at expected path: {}\n\
             Please ensure your nomos-circuits directory has the correct structure for circuit '{circuit_name}'",
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
    let base_dir = nomos_circuits_dir();
    let proving_key_path = base_dir.join(circuit_name).join("proving_key.zkey");

    if proving_key_path.is_file() {
        proving_key_path
    } else {
        panic!(
            "Proving key not found at expected path: {}\n\
             Please ensure your nomos-circuits directory has the correct structure for circuit '{circuit_name}'",
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
    let base_dir = nomos_circuits_dir();
    let verification_key_path = base_dir.join(circuit_name).join("verification_key.json");

    if verification_key_path.is_file() {
        verification_key_path
    } else {
        panic!(
            "Verification key not found at expected path: {}\n\
             Please ensure your nomos-circuits directory has the correct structure for circuit '{circuit_name}'",
            verification_key_path.display()
        )
    }
}

/// Generates a placeholder verification key in the build output directory.
///
/// # Panics
///
/// Panics if the `OUT_DIR` environment variable is not set (which happens if
/// called outside of a build script) or if the file system is read-only.
#[must_use]
pub fn dummy_verification_key_path() -> PathBuf {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let dummy_path = out_dir.join("dummy_vk.json");
    let dummy_content = r#"{"protocol": "dummy"}"#;

    std::fs::write(&dummy_path, dummy_content).expect("Failed to write dummy VK");
    dummy_path
}
