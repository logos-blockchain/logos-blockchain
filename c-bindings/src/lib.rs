#![allow(
    clippy::undocumented_unsafe_blocks,
    reason = "Well, this is gonna be a shit show of unsafe calls..."
)]

pub mod api;
mod errors;
mod macros;
mod node;
mod result;

pub use errors::OperationStatus;
pub use node::LogosBlockchainNode;
pub(crate) use result::{PointerResult, ValueResult};
