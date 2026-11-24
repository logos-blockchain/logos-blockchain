//! Lists the era files of the default deployment, so that the binary embeds
//! every one of them: each `.yaml` file in
//! `src/config/deployment/era/schedule/`, keyed by the path the deployment's
//! main file includes it by.

use std::{env, fmt::Write as _, fs, path::PathBuf};

/// The directory of the default deployment's main file, relative to the
/// crate's manifest.
const DEPLOYMENT_DIR: &str = "src/config/deployment";
/// The directory of its era files, relative to the main file, which includes
/// each by its path from there.
const ERA_FILES_DIR: &str = "era/schedule";

fn main() {
    // A directory is scanned as a whole, so adding or removing an era file
    // runs this script again.
    println!("cargo:rerun-if-changed={DEPLOYMENT_DIR}/{ERA_FILES_DIR}");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let era_files = {
        let mut unsorted_era_files: Vec<_> =
            fs::read_dir(manifest_dir.join(DEPLOYMENT_DIR).join(ERA_FILES_DIR))
                .expect("The default deployment's era files directory should be readable.")
                .map(|entry| {
                    entry
                        .expect("The default deployment's era files should be listable.")
                        .path()
                })
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "yaml")
                })
                .collect();
        // The directory's order is unspecified: sorted, the output only changes
        // with the files.
        unsorted_era_files.sort();
        unsorted_era_files
    };

    // An expression of type `&[(&str, &str)]`, embedding each file by its
    // absolute path, since the expression is included from `OUT_DIR`.
    let mut serialized_eras = String::from("&[\n");
    for era_file in era_files {
        let file_name = era_file
            .file_name()
            .expect("A directory entry has a name.")
            .to_string_lossy();
        let include_path = format!("{ERA_FILES_DIR}/{file_name}");
        let absolute_path = era_file.display().to_string();
        writeln!(
            serialized_eras,
            "    ({include_path:?}, include_str!({absolute_path:?})),"
        )
        .unwrap();
    }
    serialized_eras.push(']');

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("serialized_eras.rs"), serialized_eras)
        .expect("The list of era files should be writable to OUT_DIR.");
}
