use std::ffi::c_char;

use lb_core::{
    mantle::NoteId,
    sdp::{self, DeclarationMessage, Locator, Locators, ProviderId, ServiceType},
};
use lb_groth16::fr_from_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;

use crate::{
    LogosBlockchainNode,
    api::sdp::post_declaration_sync,
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

pub const KEY_SIZE: usize = 32;

#[repr(C)]
#[derive(Default)]
pub struct DeclarationId(pub [u8; KEY_SIZE]);

impl From<sdp::DeclarationId> for DeclarationId {
    fn from(id: sdp::DeclarationId) -> Self {
        Self(id.0)
    }
}

unsafe fn parse_provider_id(ptr: *const u8) -> Result<ProviderId, OperationStatus> {
    let bytes: [u8; KEY_SIZE] = unsafe { std::slice::from_raw_parts(ptr, KEY_SIZE) }
        .try_into()
        .expect("slice is exactly KEY_SIZE bytes");
    ProviderId::try_from(bytes).map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "Invalid `provider_id` bytes.",
        )
    })
}

unsafe fn parse_zk_id(ptr: *const u8) -> Result<ZkPublicKey, OperationStatus> {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, KEY_SIZE) };
    fr_from_bytes(bytes).map(ZkPublicKey::new).map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "Invalid `zk_id` bytes.",
        )
    })
}

unsafe fn parse_locked_note_id(ptr: *const u8) -> Result<NoteId, OperationStatus> {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, KEY_SIZE) };
    fr_from_bytes(bytes).map(NoteId).map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "Invalid `locked_note_id` bytes.",
        )
    })
}

unsafe fn parse_locators(ptrs: *const *const c_char, len: usize) -> StatusResult<Locators> {
    let locator_ptrs = unsafe { std::slice::from_raw_parts(ptrs, len) };
    let mut parsed = Vec::with_capacity(len);
    for (i, &ptr) in locator_ptrs.iter().enumerate() {
        if ptr.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                format!("Null pointer at `locators[{i}]`."),
            ));
        }
        let c_str = unsafe { std::ffi::CStr::from_ptr(ptr) };
        let Ok(s) = c_str.to_str() else {
            return Err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("`locators[{i}]` is not valid UTF-8."),
            ));
        };
        let Ok(addr) = s.parse::<Locator>() else {
            return Err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("`locators[{i}]` is not a valid locator."),
            ));
        };
        parsed.push(addr);
    }
    let Ok(locators) = parsed.try_into() else {
        return Err(OperationStatus::error(
            OperationStatusCode::ValidationError,
            "Cannot use empty list of locators.",
        ));
    };
    Ok(locators)
}

/// Joins the Blend network as a core node by posting a service declaration.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `provider_id`: A non-null pointer to 32 bytes representing the Ed25519
///   provider public key.
/// - `zk_id`: A non-null pointer to 32 bytes representing the ZK public key.
/// - `locked_note_id`: A non-null pointer to 32 bytes representing the locked
///   note ID.
/// - `locators`: A pointer to an array of locator C strings. May be null if
///   `locators_len` is 0.
/// - `locators_len`: Number of entries in the `locators` array.
///
/// # Returns
///
/// A [`FfiStatusResult`] containing the declaration ID as a 32-byte array on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_join_as_core_node(
    node: *const LogosBlockchainNode,
    provider_id: *const u8,
    zk_id: *const u8,
    locked_note_id: *const u8,
    locators: *const *const c_char,
    locators_len: usize,
) -> FfiStatusResult<DeclarationId> {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(provider_id);
    return_error_if_null_pointer!(zk_id);
    return_error_if_null_pointer!(locked_note_id);
    if locators_len > 0 {
        return_error_if_null_pointer!(locators);
    }

    let provider_id = unwrap_or_return_error!(unsafe { parse_provider_id(provider_id) });
    let zk_id = unwrap_or_return_error!(unsafe { parse_zk_id(zk_id) });
    let locked_note_id = unwrap_or_return_error!(unsafe { parse_locked_note_id(locked_note_id) });
    let locators = unwrap_or_return_error!(unsafe { parse_locators(locators, locators_len) });

    let node = unsafe { &*node };

    let join_blend_as_core_node_message = DeclarationMessage {
        service_type: ServiceType::BlendNetwork,
        locators,
        provider_id,
        zk_id,
        locked_note_id,
    };
    post_declaration_sync(node, join_blend_as_core_node_message)
        .map(DeclarationId::from)
        .into()
}
