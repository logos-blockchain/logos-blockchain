//! Shared inputs and runtime information for blockchain test integrations.
//! This crate does not depend on an implementation adapter or Cucumber.

pub mod preparation;
pub mod runtime_info;
mod unique_persistent;

pub use unique_persistent::{
    get_reserved_available_tcp_port, get_reserved_available_udp_port, hash_str,
    release_reserved_port_block, unique_test_context,
};
