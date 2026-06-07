use std::path::Path;

use crate::{cli::KeygenArgs, run_commands::utils::load_or_create_signing_key};

/// Create or load a local sequencer signing key and print its public key.
pub(crate) fn run_keygen(args: &KeygenArgs) {
    let key = load_or_create_signing_key(Path::new(&args.key_path));
    println!("key_path={}", args.key_path);
    println!("public_key={}", hex::encode(key.public_key().to_bytes()));
}
