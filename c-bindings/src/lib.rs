#![allow(
    clippy::undocumented_unsafe_blocks,
    reason = "Well, this is gonna be a shit show of unsafe calls..."
)]

pub mod api;
mod callbacks;
mod errors;
#[cfg(test)]
mod ffi_safety_tests;
pub(crate) mod logging;
mod macros;
mod node;
mod option;
mod result;

pub use errors::{OperationStatus, OperationStatusCode, free_operation_status};
pub use node::LogosBlockchainNode;
pub use result::{FfiResult, FfiStatusResult, StatusResult};
