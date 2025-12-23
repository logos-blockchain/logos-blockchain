use std::{
    ffi::{CString, c_char},
    os::raw::c_void,
};

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

pub fn block_subscribe_(
    node: &NomosNode,
    mut callback_per_block: Box<dyn FnMut(*const c_char) + Send + Sync>,
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

type CCallback = unsafe extern "C" fn(user_data: *const c_char);

unsafe extern "C" fn trampoline(callback: *mut c_void, block: *const c_char) {
    let closure_ptr = callback.cast::<Box<dyn FnMut(*const c_char)>>();
    let closure = unsafe { &mut *closure_ptr };
    closure(block);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn block_subscribe(node: *const NomosNode, callback_per_block: CCallback) {
    if node.is_null() {
        eprintln!("Received a null `node` pointer. Exiting.");
        return;
    }
    let node = unsafe { &*node };
    let callback_per_block = Box::new(move |block: *const c_char| unsafe {
        #[expect(
            clippy::fn_to_numeric_cast_any,
            reason = "trampoline method need to cast to void types"
        )]
        trampoline(callback_per_block as *mut c_void, block);
    });
    block_subscribe_(node, callback_per_block);
}
