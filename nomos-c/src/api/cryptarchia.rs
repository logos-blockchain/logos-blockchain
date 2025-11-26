use chain_service::CryptarchiaInfo;
use cryptarchia_engine::State;

use crate::{NomosNode, errors::OperationStatus};

#[repr(C)]
pub enum StateC {
    Bootstrapping = 0x0,
    Online = 0x1,
}

impl From<State> for StateC {
    fn from(value: State) -> Self {
        match value {
            State::Bootstrapping => Self::Bootstrapping,
            State::Online => Self::Online,
        }
    }
}

pub type HashC = [u8; 32];
pub type HeaderIdC = HashC;

#[repr(C)]
pub struct CryptarchiaInfoC {
    pub lib: HeaderIdC,
    pub tip: HeaderIdC,
    pub slot: u64,
    pub height: u64,
    pub mode: StateC,
}

impl From<CryptarchiaInfo> for CryptarchiaInfoC {
    fn from(value: CryptarchiaInfo) -> Self {
        Self {
            lib: value.lib.into(),
            tip: value.tip.into(),
            slot: u64::from(value.slot),
            height: value.height,
            mode: StateC::from(value.mode),
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
) -> Result<CryptarchiaInfo, OperationStatus> {
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

#[unsafe(no_mangle)]
/// Get the current Cryptarchia info.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`NomosNode`].
/// - `output_cryptarchia_info`: A non-null pointer to a [`CryptarchiaInfoC`]
///   struct where the output Cryptarchia info will be written.
///
/// # Returns
///
/// An [`OperationStatus`] indicating success or the specific error encountered.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that all pointers are non-null and point to valid
/// memory.
pub unsafe extern "C" fn get_cryptarchia_info(
    node: *const NomosNode,
    output_cryptarchia_info: *mut CryptarchiaInfoC,
) -> OperationStatus {
    if node.is_null() {
        eprintln!("[get_cryptarchia_info] Received a null `node` pointer. Exiting.");
        return OperationStatus::NullPtr;
    }
    if output_cryptarchia_info.is_null() {
        eprintln!(
            "[get_cryptarchia_info] Received a null `output_cryptarchia_info` pointer. Exiting."
        );
        return OperationStatus::NullPtr;
    }
    let node = unsafe { &*node };
    match get_cryptarchia_info_sync(node) {
        Ok(cryptarchia_info) => {
            let cryptarchia_info_c = CryptarchiaInfoC::from(cryptarchia_info);
            unsafe {
                *output_cryptarchia_info = cryptarchia_info_c;
            };
            OperationStatus::Ok
        }
        Err(error) => error,
    }
}
