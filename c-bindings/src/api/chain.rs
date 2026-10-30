use std::ffi::{CString, c_char};

use lb_c_macros::panic_to_error;

use crate::{
    LogosBlockchainNode, OperationStatus, errors::OperationStatusCode, result::FfiStatusResult,
    return_error_if_null_pointer,
};

/// Result type for [`get_chain_id`]. On success, `value` is a pointer to a
/// NUL-terminated C string holding the chain ID.
pub type FfiGetChainIdResult = FfiStatusResult<*mut c_char>;

/// Returns the chain ID of the deployment the given node was started with.
///
/// The chain ID is fixed by the node's deployment settings and never changes
/// while the node runs, so it is captured on the node handle at startup rather
/// than queried from a running service.
///
/// # Arguments
///
/// - `node`: A pointer to the node returned by
///   [`start_lb_node`](super::lifecycle::start_lb_node).
///
/// # Returns
///
/// A [`FfiGetChainIdResult`] containing a pointer to an allocated C string on
/// success, or an [`OperationStatus`] error on failure. A chain ID that cannot
/// be represented as a C string — one carrying an interior NUL byte — fails
/// with [`OperationStatusCode::RuntimeError`].
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
pub unsafe extern "C" fn get_chain_id(node: *const LogosBlockchainNode) -> FfiGetChainIdResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };

    let Some(chain_id) = node.chain_id() else {
        return FfiGetChainIdResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "The chain ID of this deployment cannot be represented as a C string.",
        ));
    };

    // Hand the caller its own copy: the node keeps ownership of its own string
    // for as long as it lives, and the caller frees this one with
    // `free_cstring`.
    match CString::new(chain_id.to_bytes()) {
        Ok(chain_id) => FfiGetChainIdResult::ok(chain_id.into_raw()),
        Err(error) => FfiGetChainIdResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}

pub mod api {
    use lb_chain_service::{CryptarchiaInfo, Epoch, State, api::CryptarchiaServiceApi};
    use lb_node::{RuntimeServiceId, generic_services::CryptarchiaService};
    use overwatch::overwatch::OverwatchHandle;

    use crate::{
        OperationStatus, OperationStatusCode, StatusResult, api::common::overwatch::ensure_ready,
    };

    type NodeChainApi = CryptarchiaServiceApi<CryptarchiaService<RuntimeServiceId>>;

    pub struct ChainFfiApi {
        api: NodeChainApi,
    }

    impl ChainFfiApi {
        /// Creates a Chain FFI Api from the service api
        ///
        /// # Errors
        ///
        /// Fails with [`OperationStatusCode::ServiceError`] if the chain
        /// service isn't ready.
        pub async fn new(
            api: NodeChainApi,
            overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
        ) -> StatusResult<Self> {
            ensure_ready::<CryptarchiaService<RuntimeServiceId>>(overwatch_handle).await?;
            Ok(Self { api })
        }

        /// Creates a Chain FFI Api, including the internal service api
        ///
        /// # Errors
        ///
        /// Fails with [`OperationStatusCode::ServiceError`] if the chain
        /// service isn't ready.
        pub async fn from_overwatch_handle(
            overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
        ) -> StatusResult<Self> {
            let api = NodeChainApi::from_overwatch_handle(overwatch_handle).await;
            Self::new(api, overwatch_handle).await
        }

        /// Gets the epoch of the current chain tip, but only once the chain is
        /// `Online`.
        ///
        /// # Errors
        ///
        /// Fails with [`OperationStatusCode::RelayError`] if the chain service
        /// can't be reached, and with [`OperationStatusCode::ServiceError`]
        /// while the chain is still bootstrapping or if the tip's epoch state
        /// can't be computed.
        pub async fn get_online_epoch(&self) -> StatusResult<Epoch> {
            let tip_slot = self.get_online_cryptarchia_info().await?.slot;
            let epoch_state = self.api.get_epoch_state(tip_slot).await?.map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to get the epoch state at slot {tip_slot:?}: {error}"),
                )
            })?;
            Ok(epoch_state.epoch)
        }

        /// Gets the chain info, but only once the chain is `Online`.
        async fn get_online_cryptarchia_info(&self) -> StatusResult<CryptarchiaInfo> {
            let cryptarchia_info = self.api.info().await?.cryptarchia_info;
            if cryptarchia_info.state != State::Online {
                return Err(OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    "The node is not online yet.",
                ));
            }
            Ok(cryptarchia_info)
        }
    }
}
