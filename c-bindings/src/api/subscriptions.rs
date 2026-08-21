use std::ffi::{CString, c_char};

use futures::StreamExt as _;
use lb_api_service::http::storage::StorageAdapter as _;
use lb_chain_service::api::CryptarchiaServiceApi;
use lb_core::{
    block::{Block as CoreBlock, BlockTransactions},
    mantle::{
        ledger::verification_mode::StandardMode,
        traits::{Hashable, StorageSize, hashable},
        transactions::{
            hash::TxHash,
            states::{Preverified, Unverified},
        },
    },
};
use lb_node::{
    ApiStorageAdapter, RocksBackend, RuntimeServiceId, SignedOps, StorageService,
    api::serializers::blocks::ApiProcessedBlockEventOwned, generic_services::CryptarchiaService,
};
use serde::Serialize;

use crate::{
    LogosBlockchainNode, OperationStatus,
    api::types::block::Block,
    callbacks::{BoxedCallback, CCallback, into_boxed_callback},
    errors::OperationStatusCode,
    logging, return_error_if_null_pointer,
};

#[derive(Serialize)]
#[serde(rename = "SignedMantleTx")]
#[derive(Clone)]
pub struct TxWithId {
    id: TxHash,
    #[serde(flatten)]
    tx: SignedOps<Unverified, StandardMode>,
}

impl Hashable for TxWithId {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> =
        |tx| <SignedOps<Unverified, StandardMode> as Hashable>::HASHER(&tx.tx);
    type Hash = <SignedOps<Unverified, StandardMode> as Hashable>::Hash;

    fn as_signing(&self) -> Vec<u8> {
        self.tx.as_signing()
    }
}

impl StorageSize for TxWithId {
    fn storage_size(&self) -> usize {
        self.tx.storage_size()
    }
}

#[must_use]
pub fn subscribe_to_new_blocks_sync(
    node: &LogosBlockchainNode,
    mut callback_per_block: BoxedCallback<*const c_char>,
) -> OperationStatus {
    let runtime_handler = node.get_runtime_handle();
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let Ok(relay) = overwatch
            .relay::<CryptarchiaService<RuntimeServiceId>>()
            .await
        else {
            return OperationStatus::error(
                OperationStatusCode::RelayError,
                "Failed to get relay to CryptarchiaService.",
            );
        };
        let Ok(storage_relay) = overwatch.relay::<StorageService>().await else {
            return OperationStatus::error(
                OperationStatusCode::RelayError,
                "Failed to get relay to StorageService.",
            );
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
                        let res: Result<Option<CoreBlock<SignedOps<Unverified, StandardMode>>>, _> =
                            ApiStorageAdapter::<RuntimeServiceId>::get_block(relay, event.block_id)
                                .await;
                        if let Ok(Some(block)) = res {
                            let txs_with_id: Vec<TxWithId> = block
                                .transactions_iter()
                                .map(|tx| TxWithId {
                                    id: tx.hash(),
                                    tx: tx.clone(),
                                })
                                .collect();
                            let block: CoreBlock<TxWithId> = CoreBlock::reconstruct(
                                block.header().clone(),
                                block.uncle_headers().clone(),
                                BlockTransactions::try_from(txs_with_id)
                                    .expect("Block should always build from valid block"),
                                *block.signature(),
                            )
                            .expect("Block should always build from valid block");
                            callback_per_block(Block::from(block).as_ptr());
                        } else {
                            logging::error!(
                                "subscribe_to_new_blocks_sync",
                                "Failed to get block {:?} from storage",
                                event.block_id
                            );
                        }
                    }
                    logging::warning!(
                        "subscribe_to_new_blocks_sync",
                        "Block stream closed, subscription to new blocks ended."
                    );
                });
                OperationStatus::OK
            }
            Err(e) => OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Failed to subscribe to blocks: {e}"),
            ),
        }
    })
}

/// Subscribes to new blocks on the blockchain and calls the provided callback
/// for each new block.
///
/// Deprecated: prefer [`subscribe_to_processed_blocks`], which additionally
/// carries the chain state per event (tip, tip slot, LIB, LIB slot) and the
/// block header id, uses the same JSON schema as the node's
/// `/cryptarchia/blocks/stream` HTTP endpoint (transaction ids at
/// `transactions[].mantle_tx.hash`), and signals the end of the stream. This
/// call will be removed once consumers have migrated.
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
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    let callback_per_block = into_boxed_callback(callback_per_block);
    subscribe_to_new_blocks_sync(node, callback_per_block)
}

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
pub fn subscribe_to_processed_blocks_sync(
    node: &LogosBlockchainNode,
    mut on_event: BoxedCallback<*const c_char>,
) -> OperationStatus {
    let runtime_handler = node.get_runtime_handle();
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let stream = match lb_api_service::http::mantle::get_new_blocks_stream::<
            SignedOps<Preverified, StandardMode>,
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
                    format!("Failed to subscribe to processed blocks: {e}"),
                );
            }
        };
        runtime_handler.spawn(async move {
            let mut stream = Box::pin(stream);
            while let Some(event) = stream.next().await {
                emit_json(&ApiProcessedBlockEventOwned::from(event), &mut on_event);
            }
            on_event(std::ptr::null());
        });
        OperationStatus::OK
    })
}

/// Subscribes to processed block events and calls `callback_per_event` for
/// each one.
///
/// Each event carries the full block along with the chain state after
/// processing it (tip, tip slot, LIB, LIB slot), serialized as JSON with the
/// same schema as the node's `/cryptarchia/blocks/stream` HTTP endpoint.
/// Transaction ids are at `transactions[].mantle_tx.hash`.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `callback_per_event`: Called with a pointer to a NUL-terminated JSON event
///   per processed block. The pointer is only valid for the duration of the
///   call — copy the data if it is needed longer. When the stream ends (e.g. on
///   node shutdown) the callback is called exactly once with NULL, after which
///   no further events are delivered; re-subscribe to keep receiving events.
///   Declared unsafe extern "C"; must be thread-safe.
///
/// # Returns
///
/// An [`OperationStatus`] indicating whether the subscription was established.
/// On error, the callback is never called.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_processed_blocks(
    node: *const LogosBlockchainNode,
    callback_per_event: CCallback<*const c_char>,
) -> OperationStatus {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    subscribe_to_processed_blocks_sync(node, into_boxed_callback(callback_per_event))
}

#[must_use]
pub fn subscribe_to_lib_blocks_sync(
    node: &LogosBlockchainNode,
    mut on_event: BoxedCallback<*const c_char>,
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
                        logging::error!(
                            "subscribe_to_lib_blocks_sync",
                            "LIB block stream failed: {e}"
                        );
                        break;
                    }
                }
            }
            on_event(std::ptr::null());
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
/// - `callback_per_event`: Called with a pointer to a NUL-terminated JSON event
///   per finalized block. The pointer is only valid for the duration of the
///   call — copy the data if it is needed longer. When the stream ends or fails
///   the callback is called exactly once with NULL, after which no further
///   events are delivered; re-subscribe to keep receiving events. Declared
///   unsafe extern "C"; must be thread-safe.
///
/// # Returns
///
/// An [`OperationStatus`] indicating whether the subscription was established.
/// On error, the callback is never called.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_lib_blocks(
    node: *const LogosBlockchainNode,
    callback_per_event: CCallback<*const c_char>,
) -> OperationStatus {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    subscribe_to_lib_blocks_sync(node, into_boxed_callback(callback_per_event))
}
