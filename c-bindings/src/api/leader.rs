use lb_c_macros::panic_to_error;

use crate::{
    LogosBlockchainNode,
    api::types::hash::TxHash,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

/// Claims available leader rewards.
///
/// This is a synchronous wrapper around [`ChainLeaderSerivceApi::claim`].
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`Result`] containing the submitted transaction hash on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn leader_claim_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<lb_core::mantle::TxHash> {
    node.call(
        "Failed to claim leader rewards",
        lb_node::api::calls::leader::claim(node.get_overwatch_handle()),
    )
    .map(|response| response.tx_hash)
}

pub type FfiLeaderClaimResult = FfiStatusResult<TxHash>;

/// Claims available leader rewards and submits the claim transaction.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// Returns a [`FfiLeaderClaimResult`] containing the submitted transaction hash
/// on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn leader_claim(node: *const LogosBlockchainNode) -> FfiLeaderClaimResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let tx_hash = unwrap_or_return_error!(leader_claim_sync(node));

    FfiLeaderClaimResult::ok(tx_hash.0)
}
