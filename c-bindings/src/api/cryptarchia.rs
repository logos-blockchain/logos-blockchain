use std::ffi::{CString, c_char};

use lb_chain_service::api::CryptarchiaServiceApi;
use lb_node::{RuntimeServiceId, generic_services::CryptarchiaService};

use crate::{
    LogosBlockchainNode,
    api::free,
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

#[repr(C)]
pub enum State {
    Bootstrapping = 0x0,
    Online = 0x1,
    NotStarted = 0x2,
}

impl State {
    const fn new(state: lb_chain_service::State, phase: lb_chain_service::PhaseTag) -> Self {
        if matches!(phase, lb_chain_service::PhaseTag::AwaitingGenesisTime) {
            return Self::NotStarted;
        }

        match state {
            lb_chain_service::State::Bootstrapping => Self::Bootstrapping,
            lb_chain_service::State::Online => Self::Online,
        }
    }
}

pub type Hash = [u8; 32];
pub type HeaderId = Hash;
pub type TxHash = Hash;
/// A note (UTXO) identifier, as 32 little-endian bytes. The FFI representation
/// of [`lb_core::mantle::NoteId`].
pub type NoteId = Hash;

/// Converts a raw pointer to a `TxHash` into a `lb_core::mantle::TxHash`.
///
/// # Parameters
///
/// - `tx_hash`: A raw pointer to a `TxHash` (32-byte array).
///
/// # Returns
///
/// - A `lb_core::mantle::TxHash` if successful, or an
///   `OperationStatusCode::ValidationError` if the conversion fails.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer.
/// The caller must ensure that the pointer is valid and points to a properly
/// initialized `TxHash`.
pub(crate) unsafe fn into_tx_hash(tx_hash: *const TxHash) -> lb_core::mantle::TxHash {
    lb_core::mantle::TxHash::from(unsafe { *tx_hash })
}

#[repr(C)]
pub struct CryptarchiaInfo {
    pub lib: HeaderId,
    pub lib_slot: u64,
    pub tip: HeaderId,
    pub slot: u64,
    pub height: u64,
    pub mode: State,
}

impl From<lb_chain_service::ChainServiceInfo> for CryptarchiaInfo {
    fn from(value: lb_chain_service::ChainServiceInfo) -> Self {
        Self {
            lib: value.cryptarchia_info.lib.into(),
            lib_slot: u64::from(value.cryptarchia_info.lib_slot),
            tip: value.cryptarchia_info.tip.into(),
            slot: u64::from(value.cryptarchia_info.slot),
            height: value.cryptarchia_info.height,
            mode: State::new(value.cryptarchia_info.state, value.phase),
        }
    }
}

/// Gets the current Cryptarchia info.
///
/// This is a synchronous wrapper around the asynchronous
/// [`cryptarchia_info`](lb_api_service::http::consensus::cryptarchia_info)
/// function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A `Result` containing the [`ChainServiceInfo`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_cryptarchia_info_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<lb_chain_service::ChainServiceInfo> {
    let runtime_handle = node.get_runtime_handle();

    let Ok(info) = runtime_handle.block_on(lb_api_service::http::consensus::cryptarchia_info(
        node.get_overwatch_handle(),
    )) else {
        return Err(OperationStatus::error(
            OperationStatusCode::RelayError,
            "Failed to get cryptarchia info.",
        ));
    };

    Ok(info)
}

pub type FfiCryptarchiaInfoResult = FfiStatusResult<*mut CryptarchiaInfo>;

/// Get the current Cryptarchia info.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
///
/// # Returns
///
/// A [`FfiCryptarchiaInfoResult`] containing a pointer to the allocated
/// [`CryptarchiaInfo`] struct on success, or an [`OperationStatus`] error on
/// failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that all pointers are non-null and point to valid
/// memory.
///
/// # Memory Management
///
/// This function allocates memory for the output [`CryptarchiaInfo`] struct.
/// The caller must free this memory using the [`free_cryptarchia_info`]
/// function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_cryptarchia_info(
    node: *const LogosBlockchainNode,
) -> FfiCryptarchiaInfoResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    let service_info = unwrap_or_return_error!(get_cryptarchia_info_sync(node));
    let c_info = CryptarchiaInfo::from(service_info);

    FfiCryptarchiaInfoResult::from_value(c_info)
}

/// Frees the memory allocated for a [`CryptarchiaInfo`] struct.
///
/// # Arguments
///
/// - `pointer`: A pointer to the [`CryptarchiaInfo`] struct to be freed.
#[unsafe(no_mangle)]
pub extern "C" fn free_cryptarchia_info(pointer: *mut CryptarchiaInfo) -> OperationStatus {
    free::<CryptarchiaInfo>(pointer)
}

/// Gets a block's events as a JSON string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`CryptarchiaServiceApi::get_block_events`] function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `header_id`: The 32-byte header ID of the block whose events to fetch.
///
/// # Returns
///
/// A `Result` containing a JSON string of the block's events on success, or an
/// [`OperationStatus`] error on failure. Returns
/// [`OperationStatusCode::NotFound`] if no block with the given header ID
/// exists.
pub(crate) fn get_block_events_sync(
    node: &LogosBlockchainNode,
    header_id: HeaderId,
) -> StatusResult<CString> {
    let runtime_handle = node.get_runtime_handle();
    let overwatch_handle = node.get_overwatch_handle();

    let events = runtime_handle.block_on(async move {
        let relay = overwatch_handle
            .relay::<CryptarchiaService<RuntimeServiceId>>()
            .await
            .map_err(|e| {
                OperationStatus::error(
                    OperationStatusCode::RelayError,
                    format!("Failed to get relay to CryptarchiaService: {e}"),
                )
            })?;
        let api =
            CryptarchiaServiceApi::<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>::new(
                relay,
            );
        api.get_block_events(lb_core::header::HeaderId::from(header_id))
            .await
            .map_err(|e| {
                OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to get block events: {e}"),
                )
            })
    })?;

    let events = events.ok_or_else(|| {
        OperationStatus::error(
            OperationStatusCode::NotFound,
            format!("No block found for header id {header_id:?}"),
        )
    })?;

    let json = serde_json::to_string(&events).map_err(|e| {
        OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to serialize block events: {e}"),
        )
    })?;

    CString::new(json).map_err(|e| {
        OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {e}"),
        )
    })
}

/// Result type for `get_block_events`. On success, `value` is a pointer to a
/// NUL-terminated C string containing the JSON-serialized events.
pub type FfiGetBlockEventsResult = FfiStatusResult<*mut c_char>;

/// Get a block's events as a JSON string.
///
/// Returns the JSON-serialized events emitted by the block with the given
/// 32-byte header ID.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
/// - `header_id`: A non-null pointer to a 32-byte header ID.
///
/// # Returns
///
/// A [`FfiGetBlockEventsResult`] containing a pointer to an allocated C string
/// (JSON events) on success, or an [`OperationStatus`] error on failure.
/// Returns [`OperationStatusCode::NotFound`] if no block with the given header
/// ID exists.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`], and that `header_id` is non-null and points to at
/// least 32 valid bytes.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_block_events(
    node: *const LogosBlockchainNode,
    header_id: *const HeaderId,
) -> FfiGetBlockEventsResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(header_id);

    let header_id = unsafe { *header_id };
    let node = unsafe { &*node };
    let json_cstring = unwrap_or_return_error!(get_block_events_sync(node, header_id));
    FfiGetBlockEventsResult::ok(json_cstring.into_raw())
}
