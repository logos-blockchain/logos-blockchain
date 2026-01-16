fn main() {
    // If building without default features, dummy verification key will be set.
    let feature_enabled = std::env::var("CARGO_FEATURE_BUILD_VERIFICATION_KEY").is_ok();

    let vk_path = if feature_enabled {
        let vk_path = circuits_utils::verification_key_path("poc");
        println!("cargo:rerun-if-changed={}", vk_path.display());
        vk_path
    } else {
        println!("cargo:warning=Building with dummy verification key (feature disabled).");
        circuits_utils::dummy_verification_key_path()
    };

    // Set the environment variable that will be used by include_bytes! in the code
    println!(
        "cargo:rustc-env=CARGO_BUILD_VERIFICATION_KEY={}",
        vk_path.display()
    );
}
