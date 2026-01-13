use std::{path::PathBuf, process::Command};

fn cargo_metadata() -> serde_json::Value {
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version=1")
        .output()
        .expect("Failed to execute `cargo metadata`");

    serde_json::from_slice(&output.stdout).expect("Failed to parse `cargo metadata` output.")
}

#[must_use]
pub fn get_target_directory() -> PathBuf {
    let metadata = cargo_metadata();
    let target_directory = metadata["target_directory"]
        .as_str()
        .expect("Failed to get the target directory..");
    PathBuf::from(target_directory)
}

#[cfg(debug_assertions)]
#[must_use]
pub const fn get_profile() -> &'static str {
    "debug"
}

#[cfg(not(debug_assertions))]
pub fn get_profile() -> &'static str {
    "release"
}

#[must_use]
pub fn get_project_identifier(crate_name: &str) -> String {
    format!("com.nomos.{crate_name}")
}

#[must_use]
pub fn get_workspace_root() -> PathBuf {
    let metadata = cargo_metadata();
    let workspace_root = metadata["workspace_root"]
        .as_str()
        .expect("Failed to get the workspace root.");
    PathBuf::from(workspace_root)
}

/// * `target_triple` - The target triple of the current build. Needs to follow
///   the standard format. E.g.: x86_64-unknown-linux-gnu, aarch64-apple-darwin,
///   etc.
pub fn get_target_directory_for_current_profile(target_triple: &str) -> Result<PathBuf, String> {
    let target_directory = get_target_directory();
    let profile = get_profile();

    // GitHub Actions format
    let target_triple_directory = target_directory.join(target_triple);
    if target_triple_directory.exists() {
        return Ok(target_triple_directory.join(profile));
    }

    // Local format
    let profile_directory = target_directory.join(profile);
    if profile_directory.exists() {
        return Ok(profile_directory);
    }

    Err("Could not find the target directory for profile.".to_owned())
}

#[must_use]
pub fn get_cargo_package_version(package_name: &str) -> String {
    let metadata = cargo_metadata();
    let packages = metadata["packages"]
        .as_array()
        .expect("Failed to get packages");
    let package = packages
        .iter()
        .find(|package| package["name"].as_str().unwrap() == package_name)
        .expect("Failed to get the package.");
    package["version"].to_string().trim_matches('"').to_owned()
}
