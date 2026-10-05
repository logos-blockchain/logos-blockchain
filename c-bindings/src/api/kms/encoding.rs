use bytes::Bytes;
use lb_core::crypto::{Digest as _, Hasher};
use lb_groth16::{fr_from_mod_bytes, fr_to_bytes};
use lb_key_management_system_service::keys::{
    PayloadEncoding, PublicKeyEncoding, SignatureEncoding,
};

use super::SigningKeyRole;

pub(super) fn encode_message(message: &[u8], role: SigningKeyRole) -> PayloadEncoding {
    match role {
        SigningKeyRole::BlendSigning => PayloadEncoding::Ed25519(Bytes::copy_from_slice(message)),
        SigningKeyRole::BlendZk => PayloadEncoding::Zk(fr_from_mod_bytes(&Hasher::digest(message))),
    }
}

pub(super) fn encode_public_key(public_key: &PublicKeyEncoding) -> String {
    match public_key {
        PublicKeyEncoding::Ed25519(key) => hex::encode(key.as_bytes()),
        PublicKeyEncoding::Zk(key) => hex::encode(fr_to_bytes(key.as_fr())),
    }
}

pub(super) fn encode_signature(signature: &SignatureEncoding) -> String {
    match signature {
        SignatureEncoding::Ed25519(signature) => hex::encode(signature.to_bytes()),
        SignatureEncoding::Zk(signature) => hex::encode(signature.as_proof().to_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ed25519_signs_the_message_as_is() {
        assert!(matches!(
            encode_message(b"message", SigningKeyRole::BlendSigning),
            PayloadEncoding::Ed25519(bytes) if bytes == b"message"[..]
        ));
    }
}
