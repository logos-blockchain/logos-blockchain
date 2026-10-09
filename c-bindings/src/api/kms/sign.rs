use std::{
    ffi::{CStr, CString, c_char},
    slice,
    time::Duration,
};

use lb_c_macros::panic_to_error;
use lb_key_management_system_service::api::KmsServiceApi;
use lb_node::{RuntimeServiceId, generic_services::KeyManagementService};
use overwatch::services::status::ServiceStatus;

use super::encoding::{encode_message, encode_public_key, encode_signature};
use crate::{
    LogosBlockchainNode, OperationStatus,
    api::{free, free_cstring},
    errors::OperationStatusCode,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

type NodeKms = KeyManagementService<RuntimeServiceId>;

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

/// Signs `message` with the KMS key `key_id`.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `key_id`: The KMS key to sign with.
/// - `message`: The bytes to sign.
///
/// # Returns
///
/// The public key and signature, hex-encoded, on success, or an
/// [`OperationStatus`] error on failure.
fn sign_message_sync(
    node: &LogosBlockchainNode,
    key_id: &str,
    message: &[u8],
) -> StatusResult<(String, String)> {
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

        let kms = {
            let relay = overwatch_handle.relay::<NodeKms>().await.map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::RelayError,
                    format!("Failed to get KMS relay: {error}"),
                )
            })?;

            KmsServiceApi::<NodeKms, RuntimeServiceId>::new(relay)
        };

        let public_key = kms.public_key(key_id.to_owned()).await.map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Failed to get public key for `{key_id}`: {error}"),
            )
        })?;
        let signature = {
            let payload = encode_message(message, &public_key)?;
            kms.sign(key_id.to_owned(), payload)
                .await
                .map_err(|error| {
                    OperationStatus::error(
                        OperationStatusCode::ServiceError,
                        format!("Failed to sign with `{key_id}`: {error}"),
                    )
                })?
        };

        Ok((encode_public_key(&public_key), encode_signature(&signature)))
    })
}

pub type FfiSignedMessageResult = FfiStatusResult<*mut SignedMessage>;

/// Signs a message with one of the node's keys.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `key_id`: The KMS key to sign with.
/// - `message`: A pointer to `message_len` bytes.
/// - `message_len`: The number of bytes in `message`. Must be zero if `message`
///   is `null`.
///
/// # Returns
///
/// A [`FfiSignedMessageResult`] containing a pointer to the allocated
/// [`SignedMessage`] on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure `key_id` is a valid NUL-terminated C string and
/// `message` points to at least `message_len` readable bytes.
///
/// # Memory Management
///
/// This function allocates the struct and both strings it holds.
/// The caller must free all of it with [`free_signed_message`].
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sign_message(
    node: *const LogosBlockchainNode,
    key_id: *const c_char,
    message: *const u8,
    message_len: usize,
) -> FfiSignedMessageResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(key_id);
    if message_len > 0 {
        return_error_if_null_pointer!(message);
    }

    let node = unsafe { &*node };
    let key_id =
        unwrap_or_return_error!(unsafe { CStr::from_ptr(key_id) }.to_str().map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("Invalid key id: {error}"),
            )
        }));
    let message: &[u8] = if message_len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(message, message_len) }
    };

    let (public_key, signature) = unwrap_or_return_error!(sign_message_sync(node, key_id, message));

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
