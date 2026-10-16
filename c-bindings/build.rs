use std::env;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=common/src/");
    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_language(cbindgen::Language::C)
        .with_parse_deps(true)
        .with_parse_include(&["logos-blockchain-c-common"])
        // Only used through its integer value, so no signature mentions it,
        // but C callers still need the constants.
        .include_item("KeyType")
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file("logos_blockchain.h");
}
