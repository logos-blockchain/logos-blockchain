use std::sync::LazyLock;

use lb_groth16::{
    Groth16PreparedVerificationKey, Groth16VerificationKey, Groth16VerificationKeyJsonDeser,
};

pub struct PoQVerifyingKey(Groth16PreparedVerificationKey);

impl AsRef<Groth16PreparedVerificationKey> for PoQVerifyingKey {
    fn as_ref(&self) -> &Groth16PreparedVerificationKey {
        &self.0
    }
}

pub static POQ_VK: LazyLock<PoQVerifyingKey> = LazyLock::new(|| {
    // note: CARGO_BUILD_VERIFICATION_KEY is injected by build.rs at compile time.
    let vk_json = include_bytes!(env!("CARGO_BUILD_VERIFICATION_KEY"));
    let groth16_vk_json: Groth16VerificationKeyJsonDeser =
        serde_json::from_slice(vk_json).expect("Key should always be valid");
    let groth16_vk: Groth16VerificationKey = groth16_vk_json
        .try_into()
        .expect("Verification key should always be valid");
    PoQVerifyingKey(groth16_vk.into_prepared())
});
