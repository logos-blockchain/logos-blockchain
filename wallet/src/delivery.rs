//! The delivery of a note to its recipient, off chain.
//!
//! Spec: [Mantle: Note Delivery](https://lip.logos.co/blockchain/raw/bedrock-v1.1-mantle-specification.html#note-delivery)

use lb_core::mantle::{Note, notes::NoteCommitment};
use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_note_encryption::{DeliveryMessage, EncapsulationKey, encrypt_note};
use rand_core::{CryptoRng, RngCore};

/// The message a sender hands the recipient of `note`: its commitment, and
/// its value and nonce encrypted to `encryption_key`.
///
/// The ciphertext carries no key. The recipient recomputes the commitment
/// with its public key as public key and as staking key, so `note` has to be
/// committed to these, as [`Note::new`] does, unless the recipient named
/// another staking key.
#[must_use]
pub fn deliver(
    note: &Note,
    encryption_key: &EncapsulationKey,
    rng: &mut (impl CryptoRng + RngCore),
) -> DeliveryMessage {
    let commitment = fr_to_bytes(&note.commitment().0);
    DeliveryMessage {
        commitment,
        ciphertext: encrypt_note(
            encryption_key,
            &commitment,
            note.value,
            &fr_to_bytes(&note.nonce),
            rng,
        ),
    }
}

/// The commitment `message` delivers the note of, `None` when its bytes are
/// not a field element: no note set holds such a commitment.
#[must_use]
pub fn delivered_commitment(message: &DeliveryMessage) -> Option<NoteCommitment> {
    fr_from_bytes(&message.commitment).ok().map(NoteCommitment)
}

#[cfg(test)]
mod tests {
    use lb_groth16::{Fr, fr_modulus};
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_note_encryption::{DecapsulationKey, decrypt_note};
    use rand::{SeedableRng as _, rngs::StdRng};

    use super::*;

    #[test]
    fn a_delivery_message_opens_to_the_value_and_the_nonce_of_its_note() {
        let sk_enc = DecapsulationKey::from_secret_key(&[7; 32]);
        let note = Note::new(1000, Fr::from(3u64), ZkPublicKey::new(Fr::from(5u64)));

        let message = deliver(
            &note,
            &sk_enc.encapsulation_key(),
            &mut StdRng::seed_from_u64(0),
        );

        assert_eq!(delivered_commitment(&message), Some(note.commitment()));
        assert_eq!(
            decrypt_note(&sk_enc, &message.commitment, &message.ciphertext),
            Some((note.value, fr_to_bytes(&note.nonce)))
        );
    }

    #[test]
    fn a_message_whose_commitment_is_not_a_field_element_delivers_nothing() {
        let sk_enc = DecapsulationKey::from_secret_key(&[7; 32]);
        let note = Note::new(1000, Fr::from(3u64), ZkPublicKey::new(Fr::from(5u64)));
        let mut message = deliver(
            &note,
            &sk_enc.encapsulation_key(),
            &mut StdRng::seed_from_u64(0),
        );
        message.commitment = fr_modulus().to_bytes_le().try_into().unwrap();

        assert_eq!(delivered_commitment(&message), None);
    }
}
