//! The note encryption against the notes of the ledger: a recipient opens a
//! ciphertext, then owns the note only if the commitment it recomputes with
//! its own keys is the one it was given.
//!
//! The keys are those of the leaf `m/154'/0'/0'/0'` of the Wallet Technical
//! Standard test vectors.

use lb_core::mantle::Note;
use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes};
use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};
use logos_blockchain_note_encryption::{DecapsulationKey, decrypt_note, encrypt_note};
use rand::{SeedableRng as _, rngs::StdRng};

const K_LOGOS: &str = "5e09bf4ce6b3f42970104a6f5940104407f98da0eb946104c13fb4f94c011f16";
const NF_SK: &str = "9301f8d0f4c13565063baf9b261c001522c47ea4d8cf54c9945b297d6da14724";
const PUBLIC_KEY: &str = "a05de446592d0396462b1788d3a01c1e4ad3a2102e58e541502b37391c7abc1c";

fn bytes(hex: &str) -> [u8; 32] {
    hex::decode(hex).unwrap().try_into().unwrap()
}

fn fr(hex: &str) -> Fr {
    fr_from_bytes(&bytes(hex)).unwrap()
}

fn recipient() -> (ZkPublicKey, DecapsulationKey) {
    (
        ZkKey::new(fr(K_LOGOS)).to_public_key(),
        DecapsulationKey::from_secret_key(&bytes(K_LOGOS)),
    )
}

// What a wallet does with a delivered note: open, then recompute the
// commitment under its own key as public key and staking key.
fn receive(
    sk_enc: &DecapsulationKey,
    public_key: ZkPublicKey,
    commitment: &[u8; 32],
    ciphertext: &[u8],
) -> Option<Note> {
    let (value, nonce) = decrypt_note(sk_enc, commitment, ciphertext)?;
    let note = Note::new(value, fr_from_bytes(&nonce).ok()?, public_key);
    (fr_to_bytes(&note.commitment().0) == *commitment).then_some(note)
}

#[test]
fn the_node_derives_the_keys_of_the_wallet_standard_vectors() {
    let key = ZkKey::new(fr(K_LOGOS));
    assert_eq!(fr_to_bytes(&key.nullifier_key()), bytes(NF_SK));
    assert_eq!(fr_to_bytes(key.to_public_key().as_fr()), bytes(PUBLIC_KEY));
}

#[test]
fn the_node_derives_the_reward_commitments_of_the_wallet_standard_vectors() {
    let public_key = ZkKey::new(fr(K_LOGOS)).to_public_key();
    for (value, nonce, commitment) in [
        (
            1000,
            "8adbe762f38ad51d1c2888054f5719365b4ef41d9b0d3ffb45c04e0f62a7722b",
            "3c14a6f0d8868bfe8b626a23fbc60829f5b8536b54b5d71a8fdcac13eaf99f18",
        ),
        (
            0x0123_4567_89ab,
            "aa6249ac6bd90570c96c60512beba512614d8d37156641e4dfb7deccbb99f317",
            "31e021cb7207b14e1f911b4653ead305821428fad26bd7ad061d31c15d6b0f0c",
        ),
    ] {
        let note = Note::new(value, fr(nonce), public_key);
        assert_eq!(fr_to_bytes(&note.commitment().0), bytes(commitment));
    }
}

#[test]
fn a_recipient_opens_its_note_and_recomputes_the_commitment_of_the_ledger() {
    let mut rng = StdRng::seed_from_u64(0);
    let (public_key, sk_enc) = recipient();
    let note = Note::new(0x0123_4567_89ab, fr(NF_SK), public_key);
    let commitment = fr_to_bytes(&note.commitment().0);

    let ciphertext = encrypt_note(
        &sk_enc.encapsulation_key(),
        &commitment,
        note.value,
        &fr_to_bytes(&note.nonce),
        &mut rng,
    );

    assert_eq!(
        receive(&sk_enc, public_key, &commitment, &ciphertext),
        Some(note)
    );
}

#[test]
fn a_note_committed_to_another_key_opens_but_is_not_owned() {
    let mut rng = StdRng::seed_from_u64(0);
    let (public_key, sk_enc) = recipient();
    let other_key = ZkKey::new(Fr::from(1u64)).to_public_key();

    // Committed to another public key, or to the right one with another
    // staking key: both open, since the ciphertext carries no key.
    let to_other_key = Note::new(7, fr(NF_SK), other_key);
    let under_other_staking_key = Note {
        stake_public_key: other_key,
        ..Note::new(7, fr(NF_SK), public_key)
    };
    for note in [to_other_key, under_other_staking_key] {
        let commitment = fr_to_bytes(&note.commitment().0);
        let ciphertext = encrypt_note(
            &sk_enc.encapsulation_key(),
            &commitment,
            note.value,
            &fr_to_bytes(&note.nonce),
            &mut rng,
        );
        assert!(decrypt_note(&sk_enc, &commitment, &ciphertext).is_some());
        assert_eq!(receive(&sk_enc, public_key, &commitment, &ciphertext), None);
    }
}
