use std::env;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed=src/");
    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_language(cbindgen::Language::C)
        // Only used through its integer value, so no signature mentions it,
        // but C callers still need the constants.
        .include_item("KeyType")
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file("logos_blockchain.h");
}
