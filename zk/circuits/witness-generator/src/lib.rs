use std::{
    io::{Error, Write as _},
    path::{Path, PathBuf},
};

use tempfile::NamedTempFile;

/// Runs the witness generator binary to generate a witness from the provided
/// inputs.
///
/// # Arguments
///
/// * `inputs_file` - The path to the file containing the public and private
///   inputs.
/// * `witness_file` - The path to the file where the witness will be written.
///
/// # Returns
///
/// A [`std::io::Result`] which contains the path to the witness file if
/// successful.
fn generate_witness_from_paths(
    inputs_file: &PathBuf,
    witness_file: &PathBuf,
    binary_path: &Path,
) -> std::io::Result<PathBuf> {
    let output = std::process::Command::new(binary_path)
        .arg(inputs_file)
        .arg(witness_file)
        .output()?;

    if !output.status.success() {
        let error_message = String::from_utf8_lossy(&output.stderr);
        return Err(Error::other(format!(
            "witness-generator command failed: {error_message}"
        )));
    }

    Ok(witness_file.to_owned())
}

/// Runs the witness generator binary to generate a witness from the provided
/// inputs.
///
/// # Note
///
/// Calls [`crate::pol_test`] underneath but hides the file handling details.
///
/// # Arguments
///
/// * `inputs` - A string containing the public and private inputs.
///
/// # Returns
///
/// A [`std::io::Result`] which contains the witness if successful.
pub fn generate_witness(inputs: &str, binary_path: &Path) -> std::io::Result<Vec<u8>> {
    let mut inputs_file = NamedTempFile::new()?;
    let witness_file = NamedTempFile::new()?;
    inputs_file.write_all(inputs.as_bytes())?;

    generate_witness_from_paths(
        &inputs_file.path().to_path_buf(),
        &witness_file.path().to_path_buf(),
        binary_path,
    )?;
    std::fs::read(witness_file.path())
}
