use std::env;

fn main() {
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=../../c-bindings/common/src/");

    cbindgen::Builder::new()
        .with_crate(env::var("CARGO_MANIFEST_DIR").unwrap())
        .with_language(cbindgen::Language::C)
        .with_cpp_compat(true)
        .with_include_guard("LOGOS_SQL_H")
        .with_parse_deps(true)
        .with_parse_include(&["logos-blockchain-c-common"])
        .include_item("SqlReadView")
        .generate()
        .expect("Unable to generate Logos SQL bindings")
        .write_to_file("logos_sql.h");
}
