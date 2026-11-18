//! The note encryption key of a ZK key, and the notes delivered to it.
//!
//! Spec: [Wallet Technical Standard: Note Encryption Key and Address](https://lip.logos.co/blockchain/raw/wallet-technical-standard.html#note-encryption-key-and-address)

use std::fmt::{Debug, Formatter};

use lb_core::mantle::Note;
use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_key_management_system_keys::keys::{
    ZkKey, ZkPublicKey, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_log_targets::kms;
use lb_note_encryption::{DecapsulationKey, DeliveryMessage, EncapsulationKey, decrypt_note};
use tokio::sync::oneshot;
use tracing::debug;

const LOG_TARGET: &str = kms::operators::ZK_NOTE_ENCRYPTION;

// The key that opens the note ciphertexts of `key`, `sk_enc`, derived from
// its secret key `k_logos`.
fn decapsulation_key(key: &ZkKey) -> DecapsulationKey {
    DecapsulationKey::from_secret_key(&fr_to_bytes(key.as_fr()))
}

// The note `message` delivers to the holder of `sk_enc` and `public_key`:
// the ciphertext opens, and its contents under `public_key` recompute the
// commitment of the message. The ciphertext carries no key: a sender gives a
// delivered note the public key of its recipient as staking key, unless the
// recipient named another, so `public_key` is tried as staking key first,
// then each of `staking_keys`.
fn open_delivery_message(
    sk_enc: &DecapsulationKey,
    public_key: ZkPublicKey,
    staking_keys: &[ZkPublicKey],
    message: &DeliveryMessage,
) -> Option<Note> {
    let (value, nonce) = decrypt_note(sk_enc, &message.commitment, &message.ciphertext)?;
    let nonce = fr_from_bytes(&nonce).ok()?;
    core::iter::once(public_key)
        .chain(staking_keys.iter().copied())
        .map(|stake_public_key| Note {
            stake_public_key,
            ..Note::new(value, nonce, public_key)
        })
        .find(|note| fr_to_bytes(&note.commitment().0) == message.commitment)
}

/// Gives the key senders encrypt the notes of this ZK key to, `pk_enc`.
pub struct DeriveNoteEncryptionKey {
    result_channel: oneshot::Sender<EncapsulationKey>,
}

impl Debug for DeriveNoteEncryptionKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeriveNoteEncryptionKey")
    }
}

impl DeriveNoteEncryptionKey {
    #[must_use]
    pub const fn new(result_channel: oneshot::Sender<EncapsulationKey>) -> Self {
        Self { result_channel }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for DeriveNoteEncryptionKey {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        if self
            .result_channel
            .send(decapsulation_key(key).encapsulation_key())
            .is_err()
        {
            debug!(target: LOG_TARGET, "Failed to send result via channel");
        }
        Ok(())
    }
}

/// Opens delivery messages with this ZK key, without the decapsulation key
/// leaving the KMS.
///
/// The result holds, for each message in order, the note it delivers to this
/// key, or `None` when the message is for another key, was altered, or opens
/// to a note committed to other keys. `staking_keys` are the staking keys the
/// holder named to senders, tried after its own public key.
pub struct OpenDeliveryMessages {
    messages: Vec<DeliveryMessage>,
    staking_keys: Vec<ZkPublicKey>,
    result_channel: oneshot::Sender<Vec<Option<Note>>>,
}

impl Debug for OpenDeliveryMessages {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "OpenDeliveryMessages")
    }
}

impl OpenDeliveryMessages {
    #[must_use]
    pub const fn new(
        messages: Vec<DeliveryMessage>,
        staking_keys: Vec<ZkPublicKey>,
        result_channel: oneshot::Sender<Vec<Option<Note>>>,
    ) -> Self {
        Self {
            messages,
            staking_keys,
            result_channel,
        }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for OpenDeliveryMessages {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let Self {
            messages,
            staking_keys,
            result_channel,
        } = *self;
        let sk_enc = decapsulation_key(key);
        let public_key = key.to_public_key();
        let notes = messages
            .iter()
            .map(|message| open_delivery_message(&sk_enc, public_key, &staking_keys, message))
            .collect();
        if result_channel.send(notes).is_err() {
            debug!(target: LOG_TARGET, "Failed to send result via channel");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::secured_key::SecuredKey as _;
    use lb_note_encryption::encrypt_note;
    use rand::{SeedableRng as _, rngs::StdRng};

    use super::*;

    async fn encryption_key(key: &ZkKey) -> EncapsulationKey {
        let (sender, receiver) = oneshot::channel();
        key.execute(DeriveNoteEncryptionKey::new(sender))
            .await
            .unwrap();
        receiver.await.unwrap()
    }

    async fn open(key: &ZkKey, messages: Vec<DeliveryMessage>) -> Vec<Option<Note>> {
        open_with(key, messages, vec![]).await
    }

    async fn open_with(
        key: &ZkKey,
        messages: Vec<DeliveryMessage>,
        staking_keys: Vec<ZkPublicKey>,
    ) -> Vec<Option<Note>> {
        let (sender, receiver) = oneshot::channel();
        key.execute(OpenDeliveryMessages::new(messages, staking_keys, sender))
            .await
            .unwrap();
        receiver.await.unwrap()
    }

    // What a sender does: commit the note, then encrypt its value and nonce
    // to the encryption key of the recipient.
    fn deliver(note: &Note, pk_enc: &EncapsulationKey) -> DeliveryMessage {
        let commitment = fr_to_bytes(&note.commitment().0);
        DeliveryMessage {
            commitment,
            ciphertext: encrypt_note(
                pk_enc,
                &commitment,
                note.value,
                &fr_to_bytes(&note.nonce),
                &mut StdRng::seed_from_u64(0),
            ),
        }
    }

    #[tokio::test]
    async fn each_key_has_its_own_encryption_key() {
        let key = ZkKey::new(Fr::from(7u64));
        assert_eq!(encryption_key(&key).await, encryption_key(&key).await);
        assert_ne!(
            encryption_key(&key).await,
            encryption_key(&ZkKey::new(Fr::from(8u64))).await
        );
    }

    #[tokio::test]
    async fn a_key_opens_the_notes_delivered_to_it_and_no_other() {
        let recipient = ZkKey::new(Fr::from(7u64));
        let other = ZkKey::new(Fr::from(8u64));
        let pk_enc = encryption_key(&recipient).await;

        let own = Note::new(1000, Fr::from(3u64), recipient.to_public_key());
        // Encrypted to the recipient, committed to another public key.
        let to_other_key = Note::new(1000, Fr::from(4u64), other.to_public_key());
        // Encrypted to the recipient, under a staking key it did not name.
        let other_staking_key = Note {
            stake_public_key: other.to_public_key(),
            ..own
        };
        // Encrypted to another wallet.
        let of_other_wallet = Note::new(1000, Fr::from(5u64), other.to_public_key());
        let mut altered = deliver(&own, &pk_enc);
        altered.commitment[0] ^= 1;

        let messages = vec![
            deliver(&own, &pk_enc),
            deliver(&to_other_key, &pk_enc),
            deliver(&other_staking_key, &pk_enc),
            deliver(&of_other_wallet, &encryption_key(&other).await),
            altered,
        ];

        assert_eq!(
            open(&recipient, messages).await,
            vec![Some(own), None, None, None, None]
        );
    }

    #[tokio::test]
    async fn a_note_under_a_named_staking_key_is_owned_once_the_key_is_named() {
        let recipient = ZkKey::new(Fr::from(7u64));
        let staking_key = ZkKey::new(Fr::from(9u64)).to_public_key();
        let pk_enc = encryption_key(&recipient).await;
        let note = Note {
            stake_public_key: staking_key,
            ..Note::new(1000, Fr::from(3u64), recipient.to_public_key())
        };
        let message = deliver(&note, &pk_enc);

        assert_eq!(open(&recipient, vec![message.clone()]).await, vec![None]);
        // Another named key does not help, the right one does.
        let other = ZkKey::new(Fr::from(10u64)).to_public_key();
        assert_eq!(
            open_with(&recipient, vec![message.clone()], vec![other]).await,
            vec![None]
        );
        assert_eq!(
            open_with(&recipient, vec![message], vec![other, staking_key]).await,
            vec![Some(note)]
        );
    }

    // Two outputs of one Operation paid to the same key: each has its own
    // nonce, commitment and ciphertext, and both are owned.
    #[tokio::test]
    async fn two_notes_paid_to_one_key_in_a_batch_are_both_owned() {
        let recipient = ZkKey::new(Fr::from(7u64));
        let pk_enc = encryption_key(&recipient).await;
        let first = Note::new(1000, Fr::from(3u64), recipient.to_public_key());
        let second = Note::new(1000, Fr::from(4u64), recipient.to_public_key());

        assert_eq!(
            open(
                &recipient,
                vec![deliver(&first, &pk_enc), deliver(&second, &pk_enc)]
            )
            .await,
            vec![Some(first), Some(second)]
        );
    }
}
