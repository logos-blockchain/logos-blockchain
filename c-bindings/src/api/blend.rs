use std::ffi::c_char;

use lb_blend_service::message::NetworkInfo;
use lb_c_macros::panic_to_error;
use lb_core::{
    mantle::NoteId,
    sdp::{self, Locator},
};
use lb_groth16::fr_from_bytes;
use lb_node::api::calls::blend::join_network;
use multiaddr::PeerId;

use crate::{
    LogosBlockchainNode,
    api::node::to_json_c_string,
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

unsafe fn parse_service_note_id(ptr: *const u8) -> Result<NoteId, OperationStatus> {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, KEY_SIZE) };
    fr_from_bytes(bytes).map(NoteId).map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "Invalid `service_note_id` bytes.",
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
/// - `service_note_id`: A pointer to 32 bytes representing the service note ID,
///   or null to let the node pick the smallest unlocked note held by its blend
///   ZK key that meets the minimum stake.
///
/// # Returns
///
/// A [`FfiStatusResult`] containing the declaration ID as a 32-byte array on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// `service_note_id` must be null or point to 32 readable bytes.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_join_as_core_node(
    node: *const LogosBlockchainNode,
    locator: *const c_char,
    service_note_id: *const u8,
) -> FfiStatusResult<DeclarationId> {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(locator);

    let locator = unwrap_or_return_error!(unsafe { parse_locator(locator) });
    let service_note_id = if service_note_id.is_null() {
        None
    } else {
        Some(unwrap_or_return_error!(unsafe {
            parse_service_note_id(service_note_id)
        }))
    };

    let node = unsafe { &*node };

    node.call(
        "Failed to join blend network",
        join_network(node.get_overwatch_handle(), locator, service_note_id),
    )
    .map(|submission| DeclarationId::from(submission.declaration_id))
    .into()
}

/// Gets the current Blend network information.
///
/// This is a synchronous wrapper around the asynchronous
/// [`blend_info`](lb_node::api::calls::blend::info) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A `Result` containing the Blend network information on success, or an
/// [`OperationStatus`] error on failure. The value is `None` when the node has
/// not joined the Blend network as a core node.
pub(crate) fn blend_info_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<Option<NetworkInfo<PeerId>>> {
    node.call(
        "Failed to get blend info",
        lb_node::api::calls::blend::info(node.get_overwatch_handle()),
    )
}

pub type FfiBlendInfoResult = FfiStatusResult<*mut c_char>;

/// Gets the current Blend network information as a JSON string.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiBlendInfoResult`] containing a pointer to an allocated C string (the
/// JSON-serialized network information) on success, or an [`OperationStatus`]
/// error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_info(node: *const LogosBlockchainNode) -> FfiBlendInfoResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let network_info = unwrap_or_return_error!(blend_info_sync(node));

    let json_network_info_c = unwrap_or_return_error!(to_json_c_string(&network_info));
    FfiBlendInfoResult::ok(json_network_info_c.into_raw())
}
