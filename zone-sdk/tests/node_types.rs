//! `node_types` should hold everything an `impl Node` signature needs.

use logos_blockchain_zone_sdk::node_types::{SignedOps, StandardMode, Unverified};

#[test]
fn node_types_covers_the_post_transaction_signature() {
    // `Node::post_transaction` takes `SignedOps<Unverified, StandardMode>`. This
    // stops compiling if one of the three is no longer re-exported.
    let _ = std::any::type_name::<SignedOps<Unverified, StandardMode>>();
}
