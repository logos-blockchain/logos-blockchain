use std::sync::LazyLock;

use lb_groth16::{
    Groth16PreparedVerificationKey, Groth16VerificationKey, Groth16VerificationKeyJsonDeser,
    verification_keys_match,
};

/// The key the node checks `PoL` proofs against. It is committed here so that
/// it changes only through a reviewed change to this repository, not with a
/// circuits release alone.
const VERIFICATION_KEY: &[u8] = include_bytes!("verification_key.json");

// The circuits release must ship the same key, or its prover would make proofs
// that this key rejects.
const _: () = assert!(
    verification_keys_match(lbc_pol_sys::artifacts::VERIFICATION_KEY, VERIFICATION_KEY),
    "the PoL verification key of the circuits release differs from `verification_key.json`"
);

pub struct PolVerifyingKey(Groth16PreparedVerificationKey);

impl AsRef<Groth16PreparedVerificationKey> for PolVerifyingKey {
    fn as_ref(&self) -> &Groth16PreparedVerificationKey {
        &self.0
    }
}

pub static POL_VK: LazyLock<PolVerifyingKey> = LazyLock::new(|| {
    let groth16_vk_json: Groth16VerificationKeyJsonDeser =
        serde_json::from_slice(VERIFICATION_KEY).expect("Key should always be valid");
    let groth16_vk: Groth16VerificationKey = groth16_vk_json
        .try_into()
        .expect("Verification key should always be valid");
    PolVerifyingKey(groth16_vk.into_prepared())
});
