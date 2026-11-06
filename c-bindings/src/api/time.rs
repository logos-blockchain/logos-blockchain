use std::ptr;

use lb_c_macros::panic_to_error;
use lb_node::TimeService;
use lb_time_service::TimeServiceMessage;
use tokio::sync::oneshot;

use crate::{
    LogosBlockchainNode,
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

#[repr(C)]
pub struct TimeInfo {
    pub genesis_time_unix: i64,
    pub current_slot: u64,
    pub current_epoch: u32,
    /// The chain's eras, in order: the first starts at genesis, at epoch 0,
    /// and an era's position in the list is its number. Points to
    /// `era_timings_len` contiguous [`EraTiming`] values, at least one.
    pub era_timings: *mut EraTiming,
    /// Number of entries in `era_timings`.
    pub era_timings_len: usize,
}

/// The timing of one era.
#[repr(C)]
pub struct EraTiming {
    /// The epoch the era starts at.
    pub first_epoch: u32,
    pub slot_duration_ms: u64,
    pub slots_per_epoch: u64,
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
    let runtime_handle = node.get_runtime_handle()?;
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
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::ChannelSendError,
                    format!("Failed to send time info request: {error}"),
                )
            })?;

        let service_info = receiver.await.map_err(|e| {
            OperationStatus::error(
                OperationStatusCode::ChannelReceiveError,
                format!("Failed to receive time info response: {e}"),
            )
        })?;
        let era_timings = service_info
            .era_schedules
            .iter()
            .map(|era| {
                let first_epoch = era.first_epoch.into_inner();
                Ok(EraTiming {
                    first_epoch,
                    slot_duration_ms: u64::try_from(era.parameters.slot_duration.as_millis())
                        .map_err(|error| {
                            OperationStatus::error(
                                OperationStatusCode::ConfigurationError,
                                format!(
                                    "The slot duration of the era starting at epoch {first_epoch} does not fit in u64 milliseconds: {error}"
                                ),
                            )
                        })?,
                    slots_per_epoch: era.parameters.epoch_length.get(),
                })
            })
            .collect::<StatusResult<Box<[EraTiming]>>>()?;
        let era_timings_len = era_timings.len();
        Ok(TimeInfo {
            genesis_time_unix: service_info
                .era_schedules
                .genesis()
                .start_time
                .unix_timestamp(),
            current_slot: u64::from(service_info.current_slot),
            current_epoch: u32::from(service_info.current_epoch),
            era_timings: Box::leak(era_timings).as_mut_ptr(),
            era_timings_len,
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
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_time_info(node: *const LogosBlockchainNode) -> FfiTimeInfoResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    let time_info = unwrap_or_return_error!(get_time_info_sync(node));

    FfiTimeInfoResult::from_value(time_info)
}

/// Frees the memory allocated for a [`TimeInfo`] struct, its era timings
/// included.
///
/// # Arguments
///
/// - `pointer`: A pointer to the [`TimeInfo`] struct to be freed.
///
/// # Safety
///
/// A non-null `pointer` must come from [`get_time_info`] and must not have
/// been freed already. Its `era_timings` and `era_timings_len` must be as they
/// were returned: they decide what is freed.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_time_info(pointer: *mut TimeInfo) -> OperationStatus {
    return_error_if_null_pointer!(pointer);
    let time_info = unsafe { Box::from_raw(pointer) };
    drop(unsafe {
        Box::from_raw(ptr::slice_from_raw_parts_mut(
            time_info.era_timings,
            time_info.era_timings_len,
        ))
    });
    OperationStatus::OK
}
