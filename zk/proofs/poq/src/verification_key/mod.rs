use std::sync::LazyLock;

use lb_groth16::{
    Groth16PreparedVerificationKey, Groth16VerificationKey, Groth16VerificationKeyJsonDeser,
    pinned_key,
};

/// SHA-256 digest of the `poq` verification key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const VERIFICATION_KEY_SHA256: &str =
    "a6898d3eb5353d1333c04f51c365edb669ab1f18c68bd9d9109fc7187bebe10f";

pub struct PoQVerifyingKey(Groth16PreparedVerificationKey);

impl AsRef<Groth16PreparedVerificationKey> for PoQVerifyingKey {
    fn as_ref(&self) -> &Groth16PreparedVerificationKey {
        &self.0
    }
}

pub static POQ_VK: LazyLock<PoQVerifyingKey> = LazyLock::new(|| {
    let vk_json = pinned_key(
        "PoQ verification key",
        lbc_poq_sys::artifacts::VERIFICATION_KEY,
        VERIFICATION_KEY_SHA256,
    );
    let groth16_vk_json: Groth16VerificationKeyJsonDeser =
        serde_json::from_slice(vk_json).expect("Key should always be valid");
    let groth16_vk: Groth16VerificationKey = groth16_vk_json
        .try_into()
        .expect("Verification key should always be valid");
    PoQVerifyingKey(groth16_vk.into_prepared())
});

#[cfg(test)]
mod tests {
    use lb_groth16::key_sha256;

    use super::*;

    #[test]
    fn embedded_verification_key_matches_pinned_digest() {
        assert_eq!(
            key_sha256(lbc_poq_sys::artifacts::VERIFICATION_KEY),
            VERIFICATION_KEY_SHA256
        );
    }
}
