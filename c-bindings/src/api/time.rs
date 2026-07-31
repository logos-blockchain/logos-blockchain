use lb_node::TimeService;
use lb_time_service::TimeServiceMessage;
use tokio::sync::oneshot;

use crate::{
    LogosBlockchainNode,
    api::free,
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

#[repr(C)]
pub struct TimeInfo {
    pub slot_duration_ms: u64,
    pub genesis_time_unix_ms: i64,
    pub current_slot: u64,
    pub current_epoch: u32,
}

/// Gets the current time service info.
///
/// This is a synchronous wrapper around the asynchronous
/// [`TimeServiceMessage::Info`] request.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A `Result` containing the [`TimeInfo`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_time_info_sync(node: &LogosBlockchainNode) -> StatusResult<TimeInfo> {
    let runtime_handle = node.get_runtime_handle();
    let overwatch_handle = node.get_overwatch_handle();

    runtime_handle.block_on(async move {
        let relay = overwatch_handle.relay::<TimeService>().await.map_err(|e| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to get relay to TimeService: {e}"),
            )
        })?;

        let (sender, receiver) = oneshot::channel();
        relay
            .send(TimeServiceMessage::Info { sender })
            .await
            .map_err(|(e, _)| {
                OperationStatus::error(
                    OperationStatusCode::ChannelSendError,
                    format!("Failed to send time info request: {e}"),
                )
            })?;

        let service_info = receiver
            .await
            .map_err(|e| {
                OperationStatus::error(
                    OperationStatusCode::ChannelReceiveError,
                    format!("Failed to receive time info response: {e}"),
                )
            })?
            .map_err(|e| {
                OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to get time info: {e}"),
                )
            })?;

        Ok(TimeInfo {
            slot_duration_ms: service_info.slot_duration_ms,
            genesis_time_unix_ms: service_info.genesis_time_unix_ms,
            current_slot: u64::from(service_info.current_slot),
            current_epoch: u32::from(service_info.current_epoch),
        })
    })
}

pub type FfiTimeInfoResult = FfiStatusResult<*mut TimeInfo>;

/// Get the current time service info.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
///
/// # Returns
///
/// A [`FfiTimeInfoResult`] containing a pointer to the allocated [`TimeInfo`]
/// struct on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`].
///
/// # Memory Management
///
/// This function allocates memory for the output [`TimeInfo`] struct. The
/// caller must free this memory using the [`free_time_info`] function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_time_info(node: *const LogosBlockchainNode) -> FfiTimeInfoResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    let time_info = unwrap_or_return_error!(get_time_info_sync(node));

    FfiTimeInfoResult::from_value(time_info)
}

/// Frees the memory allocated for a [`TimeInfo`] struct.
///
/// # Arguments
///
/// - `pointer`: A pointer to the [`TimeInfo`] struct to be freed.
#[unsafe(no_mangle)]
pub extern "C" fn free_time_info(pointer: *mut TimeInfo) -> OperationStatus {
    free::<TimeInfo>(pointer)
}
