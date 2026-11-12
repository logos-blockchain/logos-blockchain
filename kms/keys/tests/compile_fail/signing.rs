//! Key material remains available without proving, and Ed25519 still supports
//! signing. This positive control also checks the imports used below.
//!
//! ```
//! use logos_blockchain_key_management_system_keys::keys::{
//!     Ed25519Key, Key, ZkKey, SecuredKey, SigningKey,
//! };
//!
//! fn requires_key_material<K: SecuredKey>() {}
//! fn requires_signing<K: SigningKey>() {}
//! requires_key_material::<Key>();
//! requires_key_material::<ZkKey>();
//! requires_signing::<Ed25519Key>();
//! ```
//!
//! ZK keys do not provide signing without the `prove` feature.
//!
//! ```compile_fail
//! use logos_blockchain_key_management_system_keys::keys::{
//!     ZkKey, SigningKey,
//! };
//!
//! fn requires_signing<K: SigningKey>() {}
//! requires_signing::<ZkKey>();
//! ```
//!
//! The mixed key enum also requires `prove` for signing.
//!
//! ```compile_fail
//! use logos_blockchain_key_management_system_keys::keys::{
//!     Key, SigningKey,
//! };
//!
//! fn requires_signing<K: SigningKey>() {}
//! requires_signing::<Key>();
//! ```
