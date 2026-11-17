use std::sync::LazyLock;

use lb_groth16::{
    Groth16PreparedVerificationKey, Groth16VerificationKey, Groth16VerificationKeyJsonDeser,
    pinned_key,
};

/// SHA-256 digest of the `signature` verification key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const VERIFICATION_KEY_SHA256: &str =
    "b6f8a7861308f0df1a7ead87f4d8158f59104738bb24a24b18904f0bbbbe0df9";

pub struct ZkSignVerifyingKey(Groth16PreparedVerificationKey);

impl AsRef<Groth16PreparedVerificationKey> for ZkSignVerifyingKey {
    fn as_ref(&self) -> &Groth16PreparedVerificationKey {
        &self.0
    }
}

pub static ZKSIGN_VK: LazyLock<ZkSignVerifyingKey> = LazyLock::new(|| {
    let vk_json = pinned_key(
        "ZkSign verification key",
        lbc_signature_sys::artifacts::VERIFICATION_KEY,
        VERIFICATION_KEY_SHA256,
    );
    let groth16_vk_json: Groth16VerificationKeyJsonDeser =
        serde_json::from_slice(vk_json).expect("Key should always be valid");
    let groth16_vk: Groth16VerificationKey = groth16_vk_json
        .try_into()
        .expect("Verification key should always be valid");
    ZkSignVerifyingKey(groth16_vk.into_prepared())
});

#[cfg(test)]
mod tests {
    use lb_groth16::key_sha256;

    use super::*;

    #[test]
    fn embedded_verification_key_matches_pinned_digest() {
        assert_eq!(
            key_sha256(lbc_signature_sys::artifacts::VERIFICATION_KEY),
            VERIFICATION_KEY_SHA256
        );
    }
}
