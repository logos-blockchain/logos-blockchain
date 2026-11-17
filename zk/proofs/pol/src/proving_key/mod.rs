use std::sync::LazyLock;

use lb_groth16::pinned_key;

/// SHA-256 digest of the `pol` proving key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const PROVING_KEY_SHA256: &str = "8df72881a425e0575fe5d8518ec9fa4430d473c165c42611418172b38d773141";

/// The embedded proving key, checked against its pinned digest on first use.
pub static POL_PK: LazyLock<&'static [u8]> = LazyLock::new(|| {
    pinned_key(
        "PoL proving key",
        lbc_pol_sys::artifacts::PROVING_KEY,
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
            key_sha256(lbc_pol_sys::artifacts::PROVING_KEY),
            PROVING_KEY_SHA256
        );
    }
}
