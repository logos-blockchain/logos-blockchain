use std::ffi::c_char;

use lb_api_service::http::storage::StorageAdapter as _;
use lb_chain_service::api::CryptarchiaServiceApi;
use lb_core::block::Block as CoreBlock;
use lb_node::{
    ApiStorageAdapter, RuntimeServiceId, SignedMantleTx, StorageService,
    generic_services::CryptarchiaService,
};
use log::warn;

use crate::{
    LogosBlockchainNode, OperationStatus,
    api::types::block::Block,
    callbacks::{BoxedCallback, CCallback, into_boxed_callback},
    return_error_if_null_pointer,
};

pub fn subscribe_to_new_blocks_sync(
    node: &LogosBlockchainNode,
    mut callback_per_block: BoxedCallback<*const c_char>,
) {
    let runtime_handler = node.get_runtime_handle();
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let Ok(relay) = overwatch
            .relay::<CryptarchiaService<RuntimeServiceId>>()
            .await
        else {
            log::error!("Failed to get relay to CryptarchiaService");
            return;
        };
        let Ok(storage_relay) = overwatch.relay::<StorageService>().await else {
            log::error!("Failed to get relay to StorageService");
            return;
        };
        let api =
            CryptarchiaServiceApi::<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>::new(
                relay,
            );
        match api.subscribe_new_blocks().await {
            Ok(mut block_stream) => {
                runtime_handler.spawn(async move {
                    while let Ok(event) = block_stream.recv().await {
                        let relay = storage_relay.clone();
                        let res: Result<Option<CoreBlock<SignedMantleTx>>, _> =
                            ApiStorageAdapter::<RuntimeServiceId>::get_block(relay, event.block_id)
                                .await;
                        if let Ok(Some(block)) = res {
                            callback_per_block(Block::from(block).as_ptr());
                        } else {
                            log::error!("Failed to get block {:?} from storage", event.block_id);
                        }
                    }
                    warn!("Block stream closed, subscription to new blocks ended.");
                });
            }
            Err(e) => {
                log::error!("Failed to subscribe to blocks: {e}");
            }
        }
    });
}

/// Subscribes to new blocks on the blockchain and calls the provided callback
/// for each new block.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `callback_per_block`: A callback function that will be called with a
///   pointer to a C string containing the JSON representation of each new
///   block. The callback is declared as unsafe extern "C" and must be
///   thread-safe.
///
/// # Returns
///
/// An [`OperationStatus`] indicating success or failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_new_blocks(
    node: *const LogosBlockchainNode,
    callback_per_block: CCallback<*const c_char>,
) -> OperationStatus {
    return_error_if_null_pointer!("subscribe_to_new_blocks", node);
    let node = unsafe { &*node };
    let callback_per_block = into_boxed_callback(callback_per_block);
    subscribe_to_new_blocks_sync(node, callback_per_block);
    OperationStatus::Ok
}
