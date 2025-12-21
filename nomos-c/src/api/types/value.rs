//! Canonical `Value` type for the C FFI.
//!
//! `nomos_core::mantle::Value` is a type alias defined in an external
//! crate. Since `cbindgen` cannot reliably emit typedefs for
//! re-exported aliases, we define the FFI-facing `Value` locally and
//! enforce at compile time that it is exactly the same type as the
//! upstream one.

// Upstream Nomos type
use nomos_core::mantle::Value as NomosValue;

// FFI-visible type (emitted by cbindgen)
pub type Value = u64;

// Compile-time type identity check
const _: fn() = || {
    let _: fn(Value) = |_: NomosValue| {};
    let _: fn(NomosValue) = |_: Value| {};
};
