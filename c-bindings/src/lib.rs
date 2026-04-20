#![allow(
    clippy::undocumented_unsafe_blocks,
    reason = "Well, this is gonna be a shit show of unsafe calls..."
)]

pub mod api;
mod callbacks;
mod errors;
mod macros;
mod node;
mod result;

pub use errors::OperationStatus;
pub use node::LogosBlockchainNode;
pub use result::{FfiResult, FfiStatusResult, StatusResult};
