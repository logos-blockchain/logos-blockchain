fn main() {
    // If building without default features, dummy verification key will be set.
    let feature_enabled = std::env::var("CARGO_FEATURE_BUILD_VERIFICATION_KEY").is_ok();

    let vk_path = if feature_enabled {
        circuits_utils::verification_key_path("zksign")
    } else {
        println!("cargo:warning=Building with dummy verification key (feature disabled).");
        circuits_utils::dummy_verification_key_path()
    };

    println!(
        "cargo:rustc-env=CARGO_BUILD_VERIFICATION_KEY={}",
        vk_path.display()
    );

    // Only watch the file if it's NOT the dummy in the OUT_DIR
    if !vk_path.starts_with(std::env::var("OUT_DIR").unwrap_or_default()) {
        println!("cargo:rerun-if-changed={}", vk_path.display());
    }
}
