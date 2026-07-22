use std::ffi::c_char;

use lb_core::{
    mantle::NoteId,
    sdp::{self, Locator},
};
use lb_groth16::fr_from_bytes;
use lb_node::{
    RuntimeServiceId,
    generic_services::blend::{BlendBroadcastSettings, BlendService},
};

use crate::{
    LogosBlockchainNode,
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

unsafe fn parse_locked_note_id(ptr: *const u8) -> Result<NoteId, OperationStatus> {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, KEY_SIZE) };
    fr_from_bytes(bytes).map(NoteId).map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "Invalid `locked_note_id` bytes.",
        )
    })
}

unsafe fn parse_locator(ptr: *const c_char) -> Result<Locator, OperationStatus> {
    let c_str = unsafe { std::ffi::CStr::from_ptr(ptr) };
    let Ok(s) = c_str.to_str() else {
        return Err(OperationStatus::error(
            OperationStatusCode::ValidationError,
            "`locator` is not valid UTF-8.",
        ));
    };
    s.parse::<Locator>().map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "`locator` is not a valid locator.",
        )
    })
}

/// Joins the Blend network as a core node.
///
/// This delegates to the Blend service, which derives the node's own
/// `provider_id` and `zk_id` internally, mirroring the `POST /blend/join` HTTP
/// endpoint.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `locator`: A non-null pointer to a locator C string.
/// - `locked_note_id`: A non-null pointer to 32 bytes representing the locked
///   note ID.
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
    locator: *const c_char,
    locked_note_id: *const u8,
) -> FfiStatusResult<DeclarationId> {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(locator);
    return_error_if_null_pointer!(locked_note_id);

    let locator = unwrap_or_return_error!(unsafe { parse_locator(locator) });
    let locked_note_id = unwrap_or_return_error!(unsafe { parse_locked_note_id(locked_note_id) });

    let node = unsafe { &*node };

    let result: StatusResult<sdp::DeclarationId> = node.get_runtime_handle().block_on(async {
        lb_api_service::http::blend::blend_join_network::<
            BlendService<RuntimeServiceId>,
            BlendBroadcastSettings<RuntimeServiceId>,
            RuntimeServiceId,
        >(node.get_overwatch_handle(), locator, locked_note_id)
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to join blend network: {error}"),
            )
        })
    });

    result.map(DeclarationId::from).into()
}
