use std::ffi::{CString, c_char};

use chain_service::api::CryptarchiaServiceApi;
use nomos_api::http::storage::StorageAdapter as _;
use nomos_core::block::Block as CoreBlock;
use nomos_node::{
    ApiStorageAdapter, RuntimeServiceId, SignedMantleTx, StorageService,
    generic_services::CryptarchiaService,
};

use crate::NomosNode;

#[repr(C)]
pub struct Block(CString); // JSON representation of a block

impl From<CoreBlock<SignedMantleTx>> for Block {
    fn from(value: CoreBlock<SignedMantleTx>) -> Self {
        Self(
            CString::new(
                serde_json::to_string(&value)
                    .expect("Serialization of a block should always succeed")
                    .into_bytes(),
            )
            .expect("Block CString should be valid utf8"),
        )
    }
}

pub fn subscribe_to_new_blocks_sync(
    node: &NomosNode,
    mut callback_per_block: BoxedCallback<*const c_char>,
) {
    let runtime_handler = node.get_runtime_handle();
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let Ok(relay) = overwatch
            .relay::<CryptarchiaService<RuntimeServiceId>>()
            .await
        else {
            eprintln!("Failed to get relay to CryptarchiaService");
            return;
        };
        let Ok(storage_relay) = overwatch.relay::<StorageService>().await else {
            eprintln!("Failed to get relay to StorageService");
            return;
        };
        let api =
            CryptarchiaServiceApi::<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>::new(
                relay,
            );
        match api.subscribe_new_blocks().await {
            Ok(mut block_stream) => {
                runtime_handler.spawn(async move {
                    loop {
                        let relay = storage_relay.clone();
                        if let Ok(header) = block_stream.recv().await {
                            let res: Result<Option<CoreBlock<SignedMantleTx>>, _> =
                                ApiStorageAdapter::<RuntimeServiceId>::get_block(relay, header)
                                    .await;
                            if let Ok(Some(block)) = res {
                                callback_per_block(Block::from(block).0.as_ptr());
                            } else {
                                eprintln!("Failed to get block {header} from storage");
                            }
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("Failed to subscribe to blocks: {e}");
            }
        }
    });
}

type CCallback<T> = unsafe extern "C" fn(data: T);
type BoxedCallback<T> = Box<dyn FnMut(T) + Send + Sync>;

fn per_block_wrapper<T: 'static>(callback: CCallback<T>) -> BoxedCallback<T> {
    Box::new(move |block: T| {
        // Safety: The callback is declared as unsafe extern "C"
        unsafe { callback(block) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_new_blocks(
    node: *const NomosNode,
    callback_per_block: CCallback<*const c_char>,
) {
    if node.is_null() {
        eprintln!("Received a null `node` pointer. Exiting.");
        return;
    }
    let node = unsafe { &*node };
    let callback_per_block = per_block_wrapper(callback_per_block);
    subscribe_to_new_blocks_sync(node, callback_per_block);
}
