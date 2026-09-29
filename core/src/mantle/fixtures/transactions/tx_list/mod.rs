mod op_refs;
mod ops;
mod signed_ops;

/// The fork digest every encoded transaction starts with: the default, as
/// transactions are not bound to a fork yet.
const FORK_DIGEST_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";
