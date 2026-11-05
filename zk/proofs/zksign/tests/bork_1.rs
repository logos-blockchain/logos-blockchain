//! Proof for finding 20261104T022437-3c6a52/1.
//!
//! The Trusted Setup Ceremony spec requires the Groth16 keys to come from a
//! multi-party Powers-of-Tau ceremony: "The Powers-of-Tau ceremony achieves
//! computational soundness against adversaries that corrupt up to (N-1)
//! participants ... provided that at least one honest participant successfully
//! erases their secret randomness." That guarantee is only meaningful when
//! there is more than one participant (N >= 2): with N == 1 the tolerated
//! corruptions are N-1 == 0, i.e. the single contributor must be trusted
//! absolutely and its toxic waste (alpha, beta, gamma, delta and the
//! contribution scalar) suffices to forge every proof under this key.
//!
//! This test reads the proving key the node actually embeds for the zksign
//! circuit (`lbc_signature_sys::artifacts::PROVING_KEY`, selected by the
//! `lbc-signature-sys` tag pinned in the workspace Cargo.toml) and counts the
//! Phase-2 contributions recorded in the zkey (snarkjs binfile section 10).
//! The shipped key records exactly ONE contribution, so the assertion that the
//! ceremony is multi-party (>= 2 contributions) fails on the code as it stands.

/// Minimal read-only parser for the snarkjs zkey "contributions" section
/// (section type 10). Returns the number of Phase-2 contributions recorded.
fn phase2_contribution_count(zkey: &[u8]) -> u32 {
    assert_eq!(&zkey[0..4], b"zkey", "not a snarkjs zkey file");
    let n_sections = u32::from_le_bytes(zkey[8..12].try_into().unwrap());

    let mut pos = 12usize;
    for _ in 0..n_sections {
        let section_type = u32::from_le_bytes(zkey[pos..pos + 4].try_into().unwrap());
        let section_size =
            u64::from_le_bytes(zkey[pos + 4..pos + 12].try_into().unwrap()) as usize;
        let body = pos + 12;
        if section_type == 10 {
            // Layout: 64-byte circuit hash (csHash), then u32 LE contribution count.
            let count_at = body + 64;
            return u32::from_le_bytes(zkey[count_at..count_at + 4].try_into().unwrap());
        }
        pos = body + section_size;
    }
    panic!("zkey has no section 10 (no Phase-2 contribution record at all)");
}

#[test]
fn shipped_zksign_key_is_from_a_multiparty_ceremony() {
    let zkey = lbc_signature_sys::artifacts::PROVING_KEY;
    let contributions = phase2_contribution_count(zkey);
    println!("embedded zksign proving key records {contributions} Phase-2 contribution(s)");
    assert!(
        contributions >= 2,
        "trusted setup is not multi-party: the embedded zksign proving key records only {contributions} \
         Phase-2 contribution(s); the spec's soundness (corrupt up to N-1 of N, >=1 honest) requires N >= 2"
    );
}
