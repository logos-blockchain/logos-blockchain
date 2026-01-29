fn main() {
    let vk_path = lb_circuits_utils::verification_key_path("poc");

    // Set the environment variable that will be used by include_bytes! in the code
    println!(
        "cargo:rustc-env=CARGO_BUILD_VERIFICATION_KEY={}",
        vk_path.display()
    );

    // Tell Cargo to rerun this build script if the verification key changes
    println!("cargo:rerun-if-changed={}", vk_path.display());
}
