use std::ffi::{CString, c_char};

use futures::StreamExt as _;
use lb_core::mantle::transactions::states::Preverified;
use lb_node::{
    RocksBackend, RuntimeServiceId, SignedMantleTx,
    api::serializers::blocks::ApiProcessedBlockEventOwned, generic_services::CryptarchiaService,
};
use serde::Serialize;

use crate::{
    LogosBlockchainNode, OperationStatus,
    callbacks::{BoxedCallback, CCallback, into_boxed_callback},
    errors::OperationStatusCode,
    return_error_if_null_pointer,
};

/// Serializes `value` as JSON and invokes `on_event` with a pointer to the
/// NUL-terminated string. The pointer is only valid for the duration of the
/// callback invocation.
fn emit_json<T: Serialize>(value: &T, on_event: &mut BoxedCallback<*const c_char>) {
    let json = CString::new(
        serde_json::to_string(value).expect("Serialization of an event should always succeed"),
    )
    .expect("Event JSON should not contain NUL bytes");
    on_event(json.as_ptr());
}

#[must_use]
pub fn subscribe_to_new_blocks_sync(
    node: &LogosBlockchainNode,
    mut on_event: BoxedCallback<*const c_char>,
    mut on_end: BoxedCallback<OperationStatus>,
) -> OperationStatus {
    let runtime_handler = node.get_runtime_handle();
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let stream = match lb_api_service::http::mantle::get_new_blocks_stream::<
            SignedMantleTx<Preverified>,
            RocksBackend,
            CryptarchiaService<RuntimeServiceId>,
            RuntimeServiceId,
        >(overwatch)
        .await
        {
            Ok(stream) => stream,
            Err(e) => {
                return OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to subscribe to new blocks: {e}"),
                );
            }
        };
        runtime_handler.spawn(async move {
            let mut stream = Box::pin(stream);
            while let Some(event) = stream.next().await {
                emit_json(&ApiProcessedBlockEventOwned::from(event), &mut on_event);
            }
            on_end(OperationStatus::OK);
        });
        OperationStatus::OK
    })
}

/// Subscribes to new blocks and calls `callback_per_event` for each processed
/// block event.
///
/// Each event carries the full block along with the chain state after
/// processing it (tip, tip slot, LIB, LIB slot), serialized as JSON with the
/// same schema as the node's `/cryptarchia/blocks/stream` HTTP endpoint.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `callback_per_event`: Called with a pointer to a NUL-terminated JSON
///   event. The pointer is only valid for the duration of the call — copy the
///   data if it is needed longer. Declared unsafe extern "C"; must be
///   thread-safe.
/// - `on_stream_end`: Called exactly once when the event stream ends (e.g. on
///   node shutdown), after which no further events are delivered. Re-subscribe
///   to keep receiving events. If the passed status carries a non-null
///   `message`, the callee must free it with
///   [`free_cstring`](super::free_cstring).
///
/// # Returns
///
/// An [`OperationStatus`] indicating whether the subscription was established.
/// On error, `on_stream_end` is never called.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_new_blocks(
    node: *const LogosBlockchainNode,
    callback_per_event: CCallback<*const c_char>,
    on_stream_end: CCallback<OperationStatus>,
) -> OperationStatus {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    subscribe_to_new_blocks_sync(
        node,
        into_boxed_callback(callback_per_event),
        into_boxed_callback(on_stream_end),
    )
}

#[must_use]
pub fn subscribe_to_lib_blocks_sync(
    node: &LogosBlockchainNode,
    mut on_event: BoxedCallback<*const c_char>,
    mut on_end: BoxedCallback<OperationStatus>,
) -> OperationStatus {
    let runtime_handler = node.get_runtime_handle();
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let stream = match lb_api_service::http::mantle::lib_block_stream(overwatch).await {
            Ok(stream) => stream,
            Err(e) => {
                return OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to subscribe to LIB blocks: {e}"),
                );
            }
        };
        runtime_handler.spawn(async move {
            let mut stream = Box::pin(stream);
            while let Some(item) = stream.next().await {
                match item {
                    Ok(block_info) => {
                        emit_json(&block_info, &mut on_event);
                    }
                    Err(e) => {
                        on_end(OperationStatus::error(
                            OperationStatusCode::ServiceError,
                            format!("LIB block stream failed: {e}"),
                        ));
                        return;
                    }
                }
            }
            on_end(OperationStatus::OK);
        });
        OperationStatus::OK
    })
}

/// Subscribes to Last Irreversible Block (LIB) updates and calls
/// `callback_per_event` for each newly finalized block.
///
/// Each event is the finalized block's info serialized as JSON with the same
/// schema as the node's `/cryptarchia/lib/stream` HTTP endpoint.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `callback_per_event`: Called with a pointer to a NUL-terminated JSON
///   event. The pointer is only valid for the duration of the call — copy the
///   data if it is needed longer. Declared unsafe extern "C"; must be
///   thread-safe.
/// - `on_stream_end`: Called exactly once when the stream ends or fails, after
///   which no further events are delivered. Re-subscribe to keep receiving
///   events. If the passed status carries a non-null `message`, the callee must
///   free it with [`free_cstring`](super::free_cstring).
///
/// # Returns
///
/// An [`OperationStatus`] indicating whether the subscription was established.
/// On error, `on_stream_end` is never called.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_lib_blocks(
    node: *const LogosBlockchainNode,
    callback_per_event: CCallback<*const c_char>,
    on_stream_end: CCallback<OperationStatus>,
) -> OperationStatus {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    subscribe_to_lib_blocks_sync(
        node,
        into_boxed_callback(callback_per_event),
        into_boxed_callback(on_stream_end),
    )
}
