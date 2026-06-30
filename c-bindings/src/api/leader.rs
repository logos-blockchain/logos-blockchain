use lb_chain_leader_service::api::ChainLeaderSerivceApi;
use lb_node::{
    RuntimeServiceId,
    generic_services::{
        ChainNetworkService, CryptarchiaLeaderService, CryptarchiaService, WalletService,
    },
};

use crate::{
    LogosBlockchainNode, OperationStatus,
    api::cryptarchia::{Hash, TxHash},
    errors::OperationStatusCode,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

type LeaderService = CryptarchiaLeaderService<
    CryptarchiaService<RuntimeServiceId>,
    ChainNetworkService<RuntimeServiceId>,
    WalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>,
    RuntimeServiceId,
>;

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
    let runtime_handle = node.get_runtime_handle();

    runtime_handle.block_on(async {
        let relay = node
            .get_overwatch_handle()
            .relay::<LeaderService>()
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::RelayError,
                    format!("Failed to get ChainLeader relay: {error}"),
                )
            })?;

        ChainLeaderSerivceApi::<LeaderService, RuntimeServiceId>::new(relay)
            .claim()
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to claim leader rewards: {error}"),
                )
            })
    })
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn leader_claim(node: *const LogosBlockchainNode) -> FfiLeaderClaimResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let tx_hash = unwrap_or_return_error!(leader_claim_sync(node));

    let Ok(tx_hash_array): Result<Hash, _> =
        tx_hash.as_signing_bytes().iter().as_slice().try_into()
    else {
        return FfiLeaderClaimResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "Failed to convert transaction hash to array.",
        ));
    };

    FfiLeaderClaimResult::ok(tx_hash_array)
}
