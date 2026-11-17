use std::sync::LazyLock;

use lb_groth16::pinned_key;

/// SHA-256 digest of the `poc` proving key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const PROVING_KEY_SHA256: &str = "5b34ff0ad8a8285fe32835bde22b262f2d59ca9b707ed8a08a70e8fd159e6b9a";

/// The embedded proving key, checked against its pinned digest on first use.
pub static POC_PK: LazyLock<&'static [u8]> = LazyLock::new(|| {
    pinned_key(
        "PoC proving key",
        lbc_poc_sys::artifacts::PROVING_KEY,
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
            key_sha256(lbc_poc_sys::artifacts::PROVING_KEY),
            PROVING_KEY_SHA256
        );
    }
}
