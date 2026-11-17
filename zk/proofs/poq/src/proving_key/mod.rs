use std::sync::LazyLock;

use lb_groth16::pinned_key;

/// SHA-256 digest of the `poq` proving key shipped with
/// `logos-blockchain-circuits` v0.5.7.
const PROVING_KEY_SHA256: &str = "640315cc3ea5c4b3366c640ad6866afd5b43e19ad50b24dc1d788d354509321a";

/// The embedded proving key, checked against its pinned digest on first use.
pub static POQ_PK: LazyLock<&'static [u8]> = LazyLock::new(|| {
    pinned_key(
        "PoQ proving key",
        lbc_poq_sys::artifacts::PROVING_KEY,
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
            key_sha256(lbc_poq_sys::artifacts::PROVING_KEY),
            PROVING_KEY_SHA256
        );
    }
}
