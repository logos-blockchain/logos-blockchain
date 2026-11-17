use std::sync::LazyLock;

use lb_groth16::{
    Groth16PreparedVerificationKey, Groth16VerificationKey, Groth16VerificationKeyJsonDeser,
    pinned_key,
};

/// SHA-256 digest of the `pol` verification key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const VERIFICATION_KEY_SHA256: &str =
    "973513d9a15e0641fa8d153ef678713f5f3dbb9883cae2320832d469b01d44a5";

pub struct PolVerifyingKey(Groth16PreparedVerificationKey);

impl AsRef<Groth16PreparedVerificationKey> for PolVerifyingKey {
    fn as_ref(&self) -> &Groth16PreparedVerificationKey {
        &self.0
    }
}

pub static POL_VK: LazyLock<PolVerifyingKey> = LazyLock::new(|| {
    let vk_json = pinned_key(
        "PoL verification key",
        lbc_pol_sys::artifacts::VERIFICATION_KEY,
        VERIFICATION_KEY_SHA256,
    );
    let groth16_vk_json: Groth16VerificationKeyJsonDeser =
        serde_json::from_slice(vk_json).expect("Key should always be valid");
    let groth16_vk: Groth16VerificationKey = groth16_vk_json
        .try_into()
        .expect("Verification key should always be valid");
    PolVerifyingKey(groth16_vk.into_prepared())
});

#[cfg(test)]
mod tests {
    use lb_groth16::key_sha256;

    use super::*;

    #[test]
    fn embedded_verification_key_matches_pinned_digest() {
        assert_eq!(
            key_sha256(lbc_pol_sys::artifacts::VERIFICATION_KEY),
            VERIFICATION_KEY_SHA256
        );
    }
}
