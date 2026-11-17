//! Note encryption for the delivery of notes off chain.
//!
//! Spec: [Common Cryptographic Components: Note Encryption Construction](https://lip.logos.co/blockchain/raw/common-cryptographic-components.html#note-encryption-construction)
//!
//! A sender encrypts the `value` and the `nonce` of a note to the encryption
//! key of its recipient, with the note commitment as associated data:
//! X-Wing (ML-KEM-768 + X25519) as specified, the key
//! `BLAKE2b-256(LOGOS_NOTE_ENC_V1 || ss || ct)`, and ChaCha20-Poly1305 with a
//! zero nonce, each key sealing one message.
//!
//! This crate works on bytes only and knows nothing of the ledger types: a
//! commitment and a note nonce are the 32-byte little-endian encodings of
//! their field elements.

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use chacha20poly1305::{AeadInPlace as _, ChaCha20Poly1305, Key, KeyInit as _, Nonce, Tag};
use hybrid_array::{Array, typenum::U64};
use rand_core::{CryptoRng, RngCore};
use x_wing::{Decapsulate as _, Decapsulator as _, KeyExport as _};

/// Size of an encryption key, the X-Wing encapsulation key.
pub const ENCAPSULATION_KEY_SIZE: usize = x_wing::ENCAPSULATION_KEY_SIZE;
/// Size of a note ciphertext: the KEM ciphertext, then the sealed plaintext.
pub const NOTE_CIPHERTEXT_SIZE: usize = KEM_CIPHERTEXT_SIZE + PLAINTEXT_SIZE + TAG_SIZE;
/// Size of a delivery message: the commitment, then the note ciphertext.
pub const DELIVERY_MESSAGE_SIZE: usize = COMMITMENT_SIZE + NOTE_CIPHERTEXT_SIZE;

const KEM_CIPHERTEXT_SIZE: usize = x_wing::CIPHERTEXT_SIZE;
const COMMITMENT_SIZE: usize = 32;
const NONCE_SIZE: usize = 32;
// value (8 bytes, little-endian) || nonce (32 bytes)
const PLAINTEXT_SIZE: usize = 8 + NONCE_SIZE;
const TAG_SIZE: usize = 16;

const NOTE_ENC_DST: &[u8] = b"LOGOS_NOTE_ENC_V1";
const WALLET_ENC_SK_DST: &[u8] = b"WALLET_ENC_SK_V1";

/// A note commitment, as the 32-byte little-endian encoding of its field
/// element.
pub type Commitment = [u8; COMMITMENT_SIZE];
/// A note nonce, as the 32-byte little-endian encoding of its field element.
pub type NoteNonce = [u8; NONCE_SIZE];
pub type NoteCiphertext = [u8; NOTE_CIPHERTEXT_SIZE];

/// The key a wallet opens its note ciphertexts with, `sk_enc`.
///
/// It is a decryption key and nothing more: it opens every ciphertext
/// addressed to the wallet, spends nothing and shows nothing of what the wallet
/// spent.
pub struct DecapsulationKey(x_wing::DecapsulationKey);

impl DecapsulationKey {
    /// The key of the ZK secret key `k_logos`, given as the 32-byte
    /// little-endian encoding of the field element:
    /// `sk_enc = BLAKE2b-256(WALLET_ENC_SK_V1 || k_logos)`.
    #[must_use]
    pub fn from_secret_key(k_logos: &[u8; 32]) -> Self {
        Self::from_seed(blake2b_256(&[WALLET_ENC_SK_DST, k_logos]))
    }

    /// The key whose 32-byte X-Wing seed is `sk_enc`.
    #[must_use]
    pub fn from_seed(sk_enc: [u8; 32]) -> Self {
        Self(x_wing::DecapsulationKey::from(sk_enc))
    }

    #[must_use]
    pub fn encapsulation_key(&self) -> EncapsulationKey {
        EncapsulationKey(self.0.encapsulation_key().clone())
    }
}

/// The key senders encrypt the notes of a wallet to, `pk_enc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncapsulationKey(x_wing::EncapsulationKey);

/// The bytes are not an encapsulation key: their length is not
/// [`ENCAPSULATION_KEY_SIZE`], or their ML-KEM-768 key fails the modulus
/// check FIPS 203 requires before encapsulating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidEncapsulationKey;

impl EncapsulationKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, InvalidEncapsulationKey> {
        x_wing::EncapsulationKey::try_from(bytes)
            .map(Self)
            .map_err(|_| InvalidEncapsulationKey)
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; ENCAPSULATION_KEY_SIZE] {
        self.0.to_bytes().into()
    }
}

/// What a sender hands over for a note: its commitment, which the recipient
/// opens the ciphertext with, then the ciphertext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryMessage {
    pub commitment: Commitment,
    pub ciphertext: NoteCiphertext,
}

impl DeliveryMessage {
    #[must_use]
    pub fn to_bytes(&self) -> [u8; DELIVERY_MESSAGE_SIZE] {
        let mut bytes = [0; DELIVERY_MESSAGE_SIZE];
        bytes[..COMMITMENT_SIZE].copy_from_slice(&self.commitment);
        bytes[COMMITMENT_SIZE..].copy_from_slice(&self.ciphertext);
        bytes
    }

    /// `None` unless `bytes` is [`DELIVERY_MESSAGE_SIZE`] long.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (commitment, ciphertext) = bytes.split_at_checked(COMMITMENT_SIZE)?;
        Some(Self {
            commitment: commitment.try_into().ok()?,
            ciphertext: ciphertext.try_into().ok()?,
        })
    }
}

/// Encrypts the `value` and the `nonce` of the note of `commitment` to
/// `pk_enc`.
#[must_use]
pub fn encrypt_note(
    pk_enc: &EncapsulationKey,
    commitment: &Commitment,
    value: u64,
    nonce: &NoteNonce,
    rng: &mut (impl CryptoRng + RngCore),
) -> NoteCiphertext {
    let mut randomness = [0; x_wing::ENCAPSULATION_RANDOMNESS_SIZE];
    rng.fill_bytes(&mut randomness);
    encrypt_note_with(pk_enc, commitment, value, nonce, randomness)
}

// `randomness` is the input of X-Wing's `EncapsulateDerand`. It must be
// uniform and used once: the AEAD nonce is zero, so the key it leads to seals
// one message only.
fn encrypt_note_with(
    pk_enc: &EncapsulationKey,
    commitment: &Commitment,
    value: u64,
    nonce: &NoteNonce,
    randomness: [u8; x_wing::ENCAPSULATION_RANDOMNESS_SIZE],
) -> NoteCiphertext {
    let (kem_ciphertext, shared_secret) = pk_enc
        .0
        .encapsulate_deterministic(&Array::<u8, U64>::from(randomness));

    let mut plaintext = [0; PLAINTEXT_SIZE];
    plaintext[..8].copy_from_slice(&value.to_le_bytes());
    plaintext[8..].copy_from_slice(nonce);
    let tag = cipher(&shared_secret, &kem_ciphertext)
        .encrypt_in_place_detached(&Nonce::default(), commitment, &mut plaintext)
        .expect("a 40-byte message is below the limit of ChaCha20-Poly1305");

    let mut ciphertext = [0; NOTE_CIPHERTEXT_SIZE];
    let (kem, sealed) = ciphertext.split_at_mut(KEM_CIPHERTEXT_SIZE);
    kem.copy_from_slice(&kem_ciphertext);
    sealed[..PLAINTEXT_SIZE].copy_from_slice(&plaintext);
    sealed[PLAINTEXT_SIZE..].copy_from_slice(&tag);
    ciphertext
}

/// Opens `ciphertext` as the note of `commitment`, giving its `value` and its
/// `nonce`.
///
/// `None` when the ciphertext is not [`NOTE_CIPHERTEXT_SIZE`] long, is for
/// another key or another commitment, or was altered. The caller still has to
/// recompute the commitment from the result and its own keys: a ciphertext
/// that opens proves nothing about the note the ledger holds.
#[must_use]
pub fn decrypt_note(
    sk_enc: &DecapsulationKey,
    commitment: &Commitment,
    ciphertext: &[u8],
) -> Option<(u64, NoteNonce)> {
    if ciphertext.len() != NOTE_CIPHERTEXT_SIZE {
        return None;
    }
    let (kem_ciphertext, sealed) = ciphertext.split_at(KEM_CIPHERTEXT_SIZE);
    let (sealed, tag) = sealed.split_at(PLAINTEXT_SIZE);
    let tag: [u8; TAG_SIZE] = tag.try_into().ok()?;
    let shared_secret = sk_enc.0.decapsulate_slice(kem_ciphertext).ok()?;

    let mut plaintext: [u8; PLAINTEXT_SIZE] = sealed.try_into().ok()?;
    cipher(&shared_secret, kem_ciphertext)
        .decrypt_in_place_detached(
            &Nonce::default(),
            commitment,
            &mut plaintext,
            &Tag::from(tag),
        )
        .ok()?;

    let (value, nonce) = plaintext.split_at(8);
    Some((
        u64::from_le_bytes(value.try_into().ok()?),
        nonce.try_into().ok()?,
    ))
}

// key = BLAKE2b-256(LOGOS_NOTE_ENC_V1 || ss || ct)
fn cipher(shared_secret: &[u8], kem_ciphertext: &[u8]) -> ChaCha20Poly1305 {
    let key = blake2b_256(&[NOTE_ENC_DST, shared_secret, kem_ciphertext]);
    ChaCha20Poly1305::new(&Key::from(key))
}

fn blake2b_256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2b::<U32>::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng as _, rngs::StdRng};
    use serde_json::Value;

    use super::*;

    // The test values of the Common Cryptographic Components annex, as their
    // generator wrote them.
    const VECTORS: &str = include_str!("../test-vectors/note-encryption.json");

    fn vectors() -> Value {
        serde_json::from_str(VECTORS).unwrap()
    }

    fn bytes(value: &Value) -> Vec<u8> {
        hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
    }

    fn array<const N: usize>(value: &Value) -> [u8; N] {
        bytes(value).try_into().unwrap()
    }

    fn wallet(vectors: &Value, name: &str) -> DecapsulationKey {
        DecapsulationKey::from_secret_key(&array(&vectors["wallets"][name]["k_logos"]))
    }

    #[test]
    fn the_wallet_keys_are_those_of_the_specification() {
        let vectors = vectors();
        for name in ["A", "B"] {
            let wallet_vector = &vectors["wallets"][name];
            let sk_enc = blake2b_256(&[WALLET_ENC_SK_DST, &bytes(&wallet_vector["k_logos"])]);
            assert_eq!(sk_enc.to_vec(), bytes(&wallet_vector["sk_enc"]));
            assert_eq!(
                wallet(&vectors, name)
                    .encapsulation_key()
                    .to_bytes()
                    .to_vec(),
                bytes(&wallet_vector["pk_enc"])
            );
        }
    }

    #[test]
    fn encrypt_note_gives_the_ciphertexts_of_the_specification() {
        let vectors = vectors();
        let pk_enc = wallet(&vectors, "A").encapsulation_key();
        let encryptions = vectors["encrypt_note"].as_object().unwrap();
        assert_eq!(encryptions.len(), 3);
        for vector in encryptions.values() {
            let ciphertext = encrypt_note_with(
                &pk_enc,
                &array(&vector["cm"]),
                vector["value"].as_u64().unwrap(),
                &array(&vector["nonce"]),
                array(&vector["eseed"]),
            );
            assert_eq!(ciphertext.to_vec(), bytes(&vector["note_ct"]));
        }
    }

    // The seven cases the specification lists for each vector.
    #[test]
    fn decrypt_note_opens_only_the_unaltered_ciphertext_of_its_wallet_and_commitment() {
        let vectors = vectors();
        let sk_a = wallet(&vectors, "A");
        let sk_b = wallet(&vectors, "B");
        for vector in vectors["encrypt_note"].as_object().unwrap().values() {
            let commitment: Commitment = array(&vector["cm"]);
            let ciphertext = bytes(&vector["note_ct"]);
            let note = (vector["value"].as_u64().unwrap(), array(&vector["nonce"]));

            assert_eq!(decrypt_note(&sk_a, &commitment, &ciphertext), Some(note));

            let mut other_commitment = commitment;
            other_commitment[0] ^= 1;
            assert_eq!(decrypt_note(&sk_a, &other_commitment, &ciphertext), None);

            assert_eq!(decrypt_note(&sk_b, &commitment, &ciphertext), None);

            let mut altered_kem = ciphertext.clone();
            altered_kem[0] ^= 1;
            assert_eq!(decrypt_note(&sk_a, &commitment, &altered_kem), None);

            let mut altered_tag = ciphertext.clone();
            altered_tag[NOTE_CIPHERTEXT_SIZE - 1] ^= 1;
            assert_eq!(decrypt_note(&sk_a, &commitment, &altered_tag), None);

            let truncated = &ciphertext[..NOTE_CIPHERTEXT_SIZE - 1];
            assert_eq!(decrypt_note(&sk_a, &commitment, truncated), None);

            let mut extended = ciphertext.clone();
            extended.push(0);
            assert_eq!(decrypt_note(&sk_a, &commitment, &extended), None);
        }
    }

    #[test]
    fn a_note_encrypted_with_fresh_randomness_opens_and_its_ciphertexts_differ() {
        let mut rng = StdRng::seed_from_u64(0);
        let sk_enc = DecapsulationKey::from_secret_key(&[7; 32]);
        let pk_enc = sk_enc.encapsulation_key();
        let (commitment, value, nonce) = ([1; 32], u64::MAX, [2; 32]);

        let first = encrypt_note(&pk_enc, &commitment, value, &nonce, &mut rng);
        let second = encrypt_note(&pk_enc, &commitment, value, &nonce, &mut rng);

        assert_ne!(first, second);
        for ciphertext in [first, second] {
            assert_eq!(
                decrypt_note(&sk_enc, &commitment, &ciphertext),
                Some((value, nonce))
            );
        }
    }

    #[test]
    fn an_encapsulation_key_round_trips_and_rejects_what_fips_203_rejects() {
        let pk_enc = DecapsulationKey::from_secret_key(&[7; 32]).encapsulation_key();
        let bytes = pk_enc.to_bytes();
        assert_eq!(EncapsulationKey::from_bytes(&bytes), Ok(pk_enc));

        assert_eq!(
            EncapsulationKey::from_bytes(&bytes[..ENCAPSULATION_KEY_SIZE - 1]),
            Err(InvalidEncapsulationKey)
        );

        // The first 12-bit coefficient of the ML-KEM key set to 4095, above
        // the modulus 3329.
        let mut out_of_range = bytes;
        out_of_range[0] = 0xff;
        out_of_range[1] |= 0x0f;
        assert_eq!(
            EncapsulationKey::from_bytes(&out_of_range),
            Err(InvalidEncapsulationKey)
        );
    }

    #[test]
    fn a_delivery_message_round_trips_at_its_size_only() {
        let message = DeliveryMessage {
            commitment: [1; 32],
            ciphertext: [2; NOTE_CIPHERTEXT_SIZE],
        };
        let bytes = message.to_bytes();
        assert_eq!(bytes.len(), 1208);
        assert_eq!(DeliveryMessage::from_bytes(&bytes), Some(message));
        assert_eq!(DeliveryMessage::from_bytes(&bytes[..1207]), None);
        assert_eq!(
            DeliveryMessage::from_bytes(&[bytes.as_slice(), &[0]].concat()),
            None
        );
    }
}
