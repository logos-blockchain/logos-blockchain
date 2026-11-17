use std::sync::LazyLock;

use lb_groth16::pinned_key;

/// SHA-256 digest of the `signature` proving key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const PROVING_KEY_SHA256: &str = "b87ac09c136022f614c2300c7aa740ae28d1a39a11e3a2e218cb3414da882ec4";

/// The embedded proving key, checked against its pinned digest on first use.
pub static ZKSIGN_PK: LazyLock<&'static [u8]> = LazyLock::new(|| {
    pinned_key(
        "ZkSign proving key",
        lbc_signature_sys::artifacts::PROVING_KEY,
        PROVING_KEY_SHA256,
    )
});

#[cfg(test)]
mod tests {
    use lb_groth16::key_sha256;

    use super::*;

    #[test]
    fn embedded_proving_key_matches_pinned_digest() {
        assert_eq!(
            key_sha256(lbc_signature_sys::artifacts::PROVING_KEY),
            PROVING_KEY_SHA256
        );
    }
}
