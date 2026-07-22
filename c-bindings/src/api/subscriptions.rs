use std::ffi::c_char;

use lb_api_service::http::storage::StorageAdapter as _;
use lb_chain_service::api::CryptarchiaServiceApi;
use lb_core::{
    block::{Block as CoreBlock, BlockTransactions},
    mantle::{StorageSize, Transaction, TransactionHasher, TxHash},
};
use lb_node::{
    ApiStorageAdapter, RuntimeServiceId, SignedMantleTx, StorageService,
    generic_services::CryptarchiaService,
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
    tx: SignedMantleTx,
}

impl Transaction for TxWithId {
    const HASHER: TransactionHasher<Self> = |tx| <SignedMantleTx as Transaction>::HASHER(&tx.tx);
    type Hash = <SignedMantleTx as Transaction>::Hash;

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
                        let res: Result<Option<CoreBlock<SignedMantleTx>>, _> =
                            ApiStorageAdapter::<RuntimeServiceId>::get_block(relay, event.block_id)
                                .await;
                        if let Ok(Some(block)) = res {
                            let txs_with_id: Vec<TxWithId> = block
                                .transactions()
                                .map(|tx| TxWithId {
                                    id: tx.hash(),
                                    tx: tx.clone(),
                                })
                                .collect();
                            let block: CoreBlock<TxWithId> = CoreBlock::reconstruct(
                                block.header().clone(),
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
