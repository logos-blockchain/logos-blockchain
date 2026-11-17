use std::sync::LazyLock;

use lb_groth16::{
    Groth16PreparedVerificationKey, Groth16VerificationKey, Groth16VerificationKeyJsonDeser,
    pinned_key,
};

/// SHA-256 digest of the `poc` verification key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const VERIFICATION_KEY_SHA256: &str =
    "94457cb5317dba09f96df09e69aa0be19a38c07d69a17cec8b0be6549606dae3";

pub struct PoCVerifyingKey(Groth16PreparedVerificationKey);

impl AsRef<Groth16PreparedVerificationKey> for PoCVerifyingKey {
    fn as_ref(&self) -> &Groth16PreparedVerificationKey {
        &self.0
    }
}

pub static POC_VK: LazyLock<PoCVerifyingKey> = LazyLock::new(|| {
    let vk_json = pinned_key(
        "PoC verification key",
        lbc_poc_sys::artifacts::VERIFICATION_KEY,
        VERIFICATION_KEY_SHA256,
    );
    let groth16_vk_json: Groth16VerificationKeyJsonDeser =
        serde_json::from_slice(vk_json).expect("Key should always be valid");
    let groth16_vk: Groth16VerificationKey = groth16_vk_json
        .try_into()
        .expect("Verification key should always be valid");
    PoCVerifyingKey(groth16_vk.into_prepared())
});

#[cfg(test)]
mod tests {
    use lb_groth16::key_sha256;

    use super::*;

    #[test]
    fn embedded_verification_key_matches_pinned_digest() {
        assert_eq!(
            key_sha256(lbc_poc_sys::artifacts::VERIFICATION_KEY),
            VERIFICATION_KEY_SHA256
        );
    }
}
