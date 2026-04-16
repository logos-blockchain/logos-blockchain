use lb_groth16::fr_from_bytes;

use crate::{
    LogosBlockchainNode,
    api::free,
    errors::OperationStatus,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

#[repr(C)]
pub enum State {
    Bootstrapping = 0x0,
    Online = 0x1,
}

impl From<lb_cryptarchia_engine::State> for State {
    fn from(value: lb_cryptarchia_engine::State) -> Self {
        match value {
            lb_cryptarchia_engine::State::Bootstrapping => Self::Bootstrapping,
            lb_cryptarchia_engine::State::Online => Self::Online,
        }
    }
}

pub type Hash = [u8; 32];
pub type HeaderId = Hash;
pub type TxHash = Hash;

/// Converts a raw pointer to a `TxHash` into a `lb_core::mantle::TxHash`.
///
/// # Parameters
///
/// - `tx_hash`: A raw pointer to a `TxHash` (32-byte array).
///
/// # Returns
///
/// - A `lb_core::mantle::TxHash` if successful, or an
///   `OperationStatus::ValidationError` if the conversion fails.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer.
/// The caller must ensure that the pointer is valid and points to a properly
/// initialized `TxHash`.
pub(crate) unsafe fn into_tx_hash(
    tx_hash: *const TxHash,
) -> Result<lb_core::mantle::TxHash, OperationStatus> {
    let tx_hash = unsafe { *tx_hash };
    fr_from_bytes(&tx_hash)
        .map(lb_core::mantle::TxHash::from)
        .map_err(|_| OperationStatus::ValidationError)
}

#[repr(C)]
pub struct CryptarchiaInfo {
    pub lib: HeaderId,
    pub tip: HeaderId,
    pub slot: u64,
    pub height: u64,
    pub mode: State,
}

impl From<lb_chain_service::CryptarchiaInfo> for CryptarchiaInfo {
    fn from(value: lb_chain_service::CryptarchiaInfo) -> Self {
        Self {
            lib: value.lib.into(),
            tip: value.tip.into(),
            slot: u64::from(value.slot),
            height: value.height,
            mode: State::from(value.mode),
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
/// A `Result` containing the [`CryptarchiaInfo`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_cryptarchia_info_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<lb_chain_service::CryptarchiaInfo> {
    let runtime_handle = node.get_runtime_handle();
    let Ok(cryptarchia_info) = runtime_handle.block_on(
        lb_api_service::http::consensus::cryptarchia_info(node.get_overwatch_handle()),
    ) else {
        log::error!("[get_cryptarchia_info_sync] Failed to get cryptarchia info. Aborting.");
        return Err(OperationStatus::RelayError);
    };
    Ok(cryptarchia_info)
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
    return_error_if_null_pointer!("get_cryptarchia_info", node);
    let node = unsafe { &*node };
    let cryptarchia_info = unwrap_or_return_error!(get_cryptarchia_info_sync(node));
    FfiCryptarchiaInfoResult::from_value(CryptarchiaInfo::from(cryptarchia_info))
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
