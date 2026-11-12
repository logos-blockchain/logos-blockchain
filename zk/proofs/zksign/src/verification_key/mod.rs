use std::sync::LazyLock;

use lb_groth16::{
    Groth16PreparedVerificationKey, Groth16VerificationKey, Groth16VerificationKeyJsonDeser,
};

pub struct ZkSignVerifyingKey(Groth16PreparedVerificationKey);

impl AsRef<Groth16PreparedVerificationKey> for ZkSignVerifyingKey {
    fn as_ref(&self) -> &Groth16PreparedVerificationKey {
        &self.0
    }
}

pub static ZKSIGN_VK: LazyLock<ZkSignVerifyingKey> = LazyLock::new(|| {
    let vk_json = include_bytes!("verification_key.json");
    let groth16_vk_json: Groth16VerificationKeyJsonDeser =
        serde_json::from_slice(vk_json).expect("Key should always be valid");
    let groth16_vk: Groth16VerificationKey = groth16_vk_json
        .try_into()
        .expect("Verification key should always be valid");
    ZkSignVerifyingKey(groth16_vk.into_prepared())
});

#[cfg(all(test, feature = "prove"))]
mod tests {
    #[test]
    fn embedded_key_matches_pinned_circuit() {
        let embedded: serde_json::Value =
            serde_json::from_slice(include_bytes!("verification_key.json")).unwrap();
        let pinned: serde_json::Value =
            serde_json::from_slice(lbc_signature_sys::artifacts::VERIFICATION_KEY).unwrap();
        assert_eq!(
            embedded, pinned,
            "update the embedded key when upgrading the circuit"
        );
    }
}
