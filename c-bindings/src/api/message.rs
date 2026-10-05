use std::{
    ffi::{CString, c_char},
    slice,
    time::Duration,
};

use bytes::Bytes;
use lb_c_macros::panic_to_error;
use lb_core::crypto::{Digest as _, Hasher};
use lb_groth16::{fr_from_mod_bytes, fr_to_bytes};
use lb_key_management_system_service::{
    api::KmsServiceApi,
    keys::{PayloadEncoding, PublicKeyEncoding, SignatureEncoding},
};
use lb_node::{
    RuntimeServiceId, config::blend::serde::Config as BlendConfig,
    generic_services::KeyManagementService,
};
use overwatch::services::status::ServiceStatus;

use crate::{
    LogosBlockchainNode, OperationStatus,
    api::{free, free_cstring},
    errors::OperationStatusCode,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

type NodeKms = KeyManagementService<RuntimeServiceId>;

/// The node key a message is signed with.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum SigningKeyRole {
    /// The Ed25519 key behind the node's Blend `provider_id`.
    BlendSigning = 0x0,
    /// The ZK key behind the node's Blend `zk_id`, which receives Blend
    /// rewards.
    BlendZk = 0x1,
}

impl TryFrom<u8> for SigningKeyRole {
    type Error = OperationStatus;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x0 => Ok(Self::BlendSigning),
            0x1 => Ok(Self::BlendZk),
            _ => Err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("Unknown signing key role {value}."),
            )),
        }
    }
}

/// The KMS key IDs behind each [`SigningKeyRole`].
pub struct SigningKeyIds {
    blend_signing: String,
    blend_zk: String,
}

impl From<&BlendConfig> for SigningKeyIds {
    fn from(config: &BlendConfig) -> Self {
        Self {
            blend_signing: config.non_ephemeral_signing_key_id.clone(),
            blend_zk: config.core.zk.secret_key_kms_id.clone(),
        }
    }
}

impl SigningKeyIds {
    fn get(&self, role: SigningKeyRole) -> &str {
        match role {
            SigningKeyRole::BlendSigning => &self.blend_signing,
            SigningKeyRole::BlendZk => &self.blend_zk,
        }
    }
}

/// A message signature and the public key that verifies it, both hex-encoded.
///
/// For an Ed25519 key, `public_key` is the 32-byte key and `signature` the
/// 64-byte signature.
///
/// For a ZK key, `public_key` is a 32-byte field element and `signature` a
/// Groth16 proof.
#[repr(C)]
pub struct SignedMessage {
    pub public_key: *mut c_char,
    pub signature: *mut c_char,
}

fn encode_message(message: &[u8], role: SigningKeyRole) -> PayloadEncoding {
    match role {
        SigningKeyRole::BlendSigning => PayloadEncoding::Ed25519(Bytes::copy_from_slice(message)),
        SigningKeyRole::BlendZk => PayloadEncoding::Zk(fr_from_mod_bytes(&Hasher::digest(message))),
    }
}

fn encode_public_key(public_key: &PublicKeyEncoding) -> String {
    match public_key {
        PublicKeyEncoding::Ed25519(key) => hex::encode(key.as_bytes()),
        PublicKeyEncoding::Zk(key) => hex::encode(fr_to_bytes(key.as_fr())),
    }
}

fn encode_signature(signature: &SignatureEncoding) -> String {
    match signature {
        SignatureEncoding::Ed25519(signature) => hex::encode(signature.to_bytes()),
        SignatureEncoding::Zk(signature) => hex::encode(signature.as_proof().to_bytes()),
    }
}

/// Signs `message` with the node key behind `role`.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `role`: The [`SigningKeyRole`] to sign with.
/// - `message`: The bytes to sign.
///
/// # Returns
///
/// The public key and signature, hex-encoded, on success, or an
/// [`OperationStatus`] error on failure. Fails right away if the KMS service is
/// not ready.
pub(crate) fn sign_message_sync(
    node: &LogosBlockchainNode,
    role: SigningKeyRole,
    message: &[u8],
) -> StatusResult<(String, String)> {
    let key_id = node.signing_key_ids().get(role).to_owned();
    let payload = encode_message(message, role);

    node.get_runtime_handle()?.block_on(async {
        let overwatch_handle = node.get_overwatch_handle();

        let mut status_watcher =
            overwatch_handle
                .status_watcher::<NodeKms>()
                .await
                .map_err(|error| {
                    OperationStatus::error(
                        OperationStatusCode::ServiceError,
                        format!("Failed to request KMS service status watcher: {error}"),
                    )
                })?;
        if let Err(status) = status_watcher
            .wait_for(ServiceStatus::Ready, Some(Duration::from_millis(100)))
            .await
        {
            return Err(OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("KMS service is not ready: {status:?}"),
            ));
        }

        let relay = overwatch_handle.relay::<NodeKms>().await.map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to get KMS relay: {error}"),
            )
        })?;
        let kms = KmsServiceApi::<NodeKms, RuntimeServiceId>::new(relay);

        let public_key = kms.public_key(key_id.clone()).await.map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Failed to get public key for `{key_id}`: {error}"),
            )
        })?;
        let signature = kms.sign(key_id.clone(), payload).await.map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Failed to sign with `{key_id}`: {error}"),
            )
        })?;

        Ok((encode_public_key(&public_key), encode_signature(&signature)))
    })
}

pub type FfiSignedMessageResult = FfiStatusResult<*mut SignedMessage>;

/// Signs a message with one of the node's keys.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `role`: A [`SigningKeyRole`] value to sign with.
/// - `message`: A pointer to `message_len` bytes. May be null only when
///   `message_len` is zero.
/// - `message_len`: The number of bytes in `message`.
///
/// # Returns
///
/// A [`FfiSignedMessageResult`] containing a pointer to the allocated
/// [`SignedMessage`] on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure `message` points to at least `message_len` readable bytes.
///
/// # Memory Management
///
/// This function allocates the struct and both strings it holds. The caller
/// must free all of it with [`free_signed_message`].
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sign_message(
    node: *const LogosBlockchainNode,
    role: u8,
    message: *const u8,
    message_len: usize,
) -> FfiSignedMessageResult {
    return_error_if_null_pointer!(node);
    if message_len > 0 {
        return_error_if_null_pointer!(message);
    }

    let role = unwrap_or_return_error!(SigningKeyRole::try_from(role));
    let node = unsafe { &*node };
    let message: &[u8] = if message_len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(message, message_len) }
    };

    let (public_key, signature) = unwrap_or_return_error!(sign_message_sync(node, role, message));

    let signed_message = SignedMessage {
        public_key: CString::new(public_key)
            .expect("Hex has no NUL bytes")
            .into_raw(),
        signature: CString::new(signature)
            .expect("Hex has no NUL bytes")
            .into_raw(),
    };
    FfiSignedMessageResult::from_value(signed_message)
}

/// Frees a [`SignedMessage`] and both strings it owns.
///
/// # Arguments
///
/// - `pointer`: A pointer to the [`SignedMessage`] to be freed.
///
/// # Safety
///
/// The pointer must come from [`sign_message`] and must not have been freed
/// already.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_signed_message(pointer: *mut SignedMessage) -> OperationStatus {
    return_error_if_null_pointer!(pointer);
    let signed_message = unsafe { &*pointer };
    unsafe { free_cstring(signed_message.public_key) };
    unsafe { free_cstring(signed_message.signature) };
    unsafe { free::<SignedMessage>(pointer) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_from_known_values() {
        assert!(matches!(
            SigningKeyRole::try_from(0),
            Ok(SigningKeyRole::BlendSigning)
        ));
        assert!(matches!(
            SigningKeyRole::try_from(1),
            Ok(SigningKeyRole::BlendZk)
        ));
    }

    #[test]
    fn role_from_unknown_value_is_a_validation_error() {
        let Err(status) = SigningKeyRole::try_from(2) else {
            panic!("2 is not a signing key role");
        };
        assert_eq!(status.code, OperationStatusCode::ValidationError);
    }

    #[test]
    fn ed25519_signs_the_message_as_is() {
        assert!(matches!(
            encode_message(b"message", SigningKeyRole::BlendSigning),
            PayloadEncoding::Ed25519(bytes) if bytes == b"message"[..]
        ));
    }
}
