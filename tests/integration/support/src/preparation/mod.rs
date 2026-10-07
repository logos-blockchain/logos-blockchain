//! Network inputs and wallet resources consumed by implementation adapters.
//!
//! These are the existing protocol data and shared deployment representation,
//! not native node configuration. Adapters own generation and translation.

mod shared_deployment;
pub mod wallet;

pub use shared_deployment::SharedDeployment;
