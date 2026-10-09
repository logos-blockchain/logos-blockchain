use bytes::Bytes;
use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_key_management_system_service::keys::{
    PayloadEncoding, PublicKeyEncoding, SignatureEncoding,
};

use crate::{OperationStatus, errors::OperationStatusCode, result::StatusResult};

/// Wraps `message` in the payload type of the key behind `public_key`.
/// ZK keys sign a field element, so for them `message` must be one.
pub(super) fn encode_message(
    message: &[u8],
    public_key: &PublicKeyEncoding,
) -> StatusResult<PayloadEncoding> {
    match public_key {
        PublicKeyEncoding::Ed25519(_) => {
            Ok(PayloadEncoding::Ed25519(Bytes::copy_from_slice(message)))
        }
        PublicKeyEncoding::Zk(_) => {
            fr_from_bytes(message)
                .map(PayloadEncoding::Zk)
                .map_err(|error| {
                    OperationStatus::error(
                        OperationStatusCode::ValidationError,
                        format!("Could not encode the ZK message: {error:?}"),
                    )
                })
        }
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
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};

    use super::*;

    #[test]
    fn ed25519_signs_the_message_as_is() {
        let public_key = PublicKeyEncoding::Ed25519(Ed25519Key::from_bytes(&[1; 32]).public_key());
        assert!(matches!(
            encode_message(b"message", &public_key),
            Ok(PayloadEncoding::Ed25519(bytes)) if bytes == b"message"[..]
        ));
    }

    #[test]
    fn zk_signs_a_field_element() {
        let public_key = PublicKeyEncoding::Zk(ZkPublicKey::new(Fr::from(1u64)));
        let element = Fr::from(7u64);
        assert!(matches!(
            encode_message(&fr_to_bytes(&element), &public_key),
            Ok(PayloadEncoding::Zk(fr)) if fr == element
        ));
    }

    #[test]
    fn zk_rejects_a_message_that_is_not_a_field_element() {
        let public_key = PublicKeyEncoding::Zk(ZkPublicKey::new(Fr::from(1u64)));
        let Err(status) = encode_message(&[0xff; 32], &public_key) else {
            panic!("`message` is not a field element");
        };
        assert_eq!(status.code, OperationStatusCode::ValidationError);
    }
}
