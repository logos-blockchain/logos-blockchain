//! Serialization contracts used by Logos blockchain components.
//!
//! [`canonical`] is the standardized Logos binary representation. [`bincode`]
//! is the configured Serde/bincode representation used by services, storage,
//! and network paths. They remain separate contracts even when a type happens
//! to have identical bytes under both encodings.

extern crate self as lb_binary_codec;

pub mod bincode;
pub mod canonical;
