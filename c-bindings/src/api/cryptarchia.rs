use std::ffi::{CString, c_char};

use lb_c_macros::panic_to_error;

use crate::{
    LogosBlockchainNode,
    api::{free, types::hash::HeaderId},
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
/// [`cryptarchia_info`](lb_node::api::calls::cryptarchia::info)
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
    node.call(
        "Failed to get cryptarchia info",
        lb_node::api::calls::cryptarchia::info(node.get_overwatch_handle()),
    )
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
#[panic_to_error]
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
///
/// # Safety
///
/// A non-null `pointer` must come from [`get_cryptarchia_info`] and must not
/// have been freed already.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_cryptarchia_info(pointer: *mut CryptarchiaInfo) -> OperationStatus {
    unsafe { free::<CryptarchiaInfo>(pointer) }
}

/// Result type for [`get_fork_digest`]. On success, `value` is a pointer to a
/// NUL-terminated C string holding the fork digest in lowercase hexadecimal.
pub type FfiGetForkDigestResult = FfiStatusResult<*mut c_char>;

/// Returns the fork digest a transaction signed now must carry: that of the era
/// the node's clock is in, in lowercase hexadecimal, as a transaction's JSON
/// carries it.
///
/// The fork digest changes when an era starts, so a caller asks for it right
/// before signing a transaction rather than keeping it.
///
/// # Arguments
///
/// - `node`: A pointer to the node returned by
///   [`start_lb_node`](super::lifecycle::start_lb_node).
///
/// # Returns
///
/// A [`FfiGetForkDigestResult`] containing a pointer to an allocated C string
/// on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// `node` must be a valid pointer to a [`LogosBlockchainNode`] that has not
/// been stopped.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_fork_digest(
    node: *const LogosBlockchainNode,
) -> FfiGetForkDigestResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    let fork_digest = unwrap_or_return_error!(node.call(
        "Failed to get fork digest",
        lb_node::api::calls::time::fork_digest_in_force(
            node.get_overwatch_handle(),
            node.fork_digests(),
        ),
    ));

    match CString::new(hex::encode(<[u8; 32]>::from(fork_digest))) {
        Ok(fork_digest) => FfiGetForkDigestResult::ok(fork_digest.into_raw()),
        Err(error) => FfiGetForkDigestResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}
