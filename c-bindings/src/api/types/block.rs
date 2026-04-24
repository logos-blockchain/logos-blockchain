use std::ffi::{CString, c_char};

use lb_core::{block::Block as CoreBlock, mantle::SignedMantleTx};

#[repr(C)]
pub struct Block(CString); // JSON representation of a block

impl Block {
    pub fn as_ptr(&self) -> *const c_char {
        self.0.as_ptr()
    }
}

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
