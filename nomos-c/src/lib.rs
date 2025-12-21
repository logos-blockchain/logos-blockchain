#![allow(
    clippy::undocumented_unsafe_blocks,
    reason = "Well, this is gonna be a shit show of unsafe calls..."
)]

pub mod api;
mod errors;
mod node;

pub use errors::OperationStatus;
pub use node::NomosNode;
