use crate::{
    NomosNode,
    api::{PointerResult, free},
    errors::OperationStatus,
};

#[repr(C)]
pub enum State {
    Bootstrapping = 0x0,
    Online = 0x1,
}

impl From<cryptarchia_engine::State> for State {
    fn from(value: cryptarchia_engine::State) -> Self {
        match value {
            cryptarchia_engine::State::Bootstrapping => Self::Bootstrapping,
            cryptarchia_engine::State::Online => Self::Online,
        }
    }
}

pub type Hash = [u8; 32];
pub type HeaderId = Hash;

#[repr(C)]
pub struct CryptarchiaInfo {
    pub lib: HeaderId,
    pub tip: HeaderId,
    pub slot: u64,
    pub height: u64,
    pub mode: State,
}

impl From<chain_service::CryptarchiaInfo> for CryptarchiaInfo {
    fn from(value: chain_service::CryptarchiaInfo) -> Self {
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
/// [`cryptarchia_info`](nomos_api::http::consensus::cryptarchia_info) function.
///
/// # Arguments
///
/// - `node`: A [`NomosNode`] instance.
///
/// # Returns
///
/// A `Result` containing the [`CryptarchiaInfo`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_cryptarchia_info_sync(
    node: &NomosNode,
) -> Result<chain_service::CryptarchiaInfo, OperationStatus> {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        eprintln!("[get_cryptarchia_info_sync] Failed to create tokio runtime. Aborting.");
        return Err(OperationStatus::RuntimeError);
    };
    let Ok(cryptarchia_info) = runtime.block_on(nomos_api::http::consensus::cryptarchia_info(
        node.get_overwatch_handle(),
    )) else {
        eprintln!("[get_cryptarchia_info_sync] Failed to get cryptarchia info. Aborting.");
        return Err(OperationStatus::RelayError);
    };
    Ok(cryptarchia_info)
}

pub type CryptarchiaInfoResult = PointerResult<CryptarchiaInfo, OperationStatus>;

/// Get the current Cryptarchia info.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`NomosNode`].
///
/// # Returns
///
/// A [`CryptarchiaInfoResult`] containing a pointer to the allocated
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
pub unsafe extern "C" fn get_cryptarchia_info(node: *const NomosNode) -> CryptarchiaInfoResult {
    if node.is_null() {
        eprintln!("[get_cryptarchia_info] Received a null `node` pointer. Exiting.");
        return CryptarchiaInfoResult::from_error(OperationStatus::NullPointer);
    }

    let node = unsafe { &*node };
    match get_cryptarchia_info_sync(node) {
        Ok(cryptarchia_info) => {
            let cryptarchia_info = CryptarchiaInfo::from(cryptarchia_info);
            CryptarchiaInfoResult::from_value(cryptarchia_info)
        }
        Err(error) => CryptarchiaInfoResult::from_error(error),
    }
}

/// Frees the memory allocated for a [`CryptarchiaInfo`] struct.
///
/// # Arguments
///
/// - `pointer`: A pointer to the [`CryptarchiaInfo`] struct to be freed.
#[unsafe(no_mangle)]
pub extern "C" fn free_cryptarchia_info(pointer: *mut CryptarchiaInfo) {
    free::<CryptarchiaInfo>(pointer);
}
