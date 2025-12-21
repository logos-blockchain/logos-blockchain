pub mod prove;
pub mod verify;

pub use self::verify::Inputs as VerifyInputs;
use crate::quota::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};

type HalfEphemeralSigningKey = [u8; ED25519_PUBLIC_KEY_SIZE / 2];

fn split_ephemeral_signing_key(
    key: Ed25519PublicKey,
) -> (HalfEphemeralSigningKey, HalfEphemeralSigningKey) {
    let key_bytes = key.as_bytes();
    (
        key_bytes[0..(ED25519_PUBLIC_KEY_SIZE / 2)]
            .try_into()
            .expect("Ephemeral signing key must be exactly 32 bytes long."),
        key_bytes[(ED25519_PUBLIC_KEY_SIZE / 2)..]
            .try_into()
            .expect("Ephemeral signing key must be exactly 32 bytes long."),
    )
}
