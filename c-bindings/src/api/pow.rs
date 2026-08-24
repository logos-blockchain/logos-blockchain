use std::ptr;

use lb_node::{PoWService, RuntimeServiceId};

use crate::{
    LogosBlockchainNode, OperationStatus,
    api::cryptarchia::Hash,
    errors::OperationStatusCode,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

/// Enables `PoW` mining.
///
/// This is a synchronous wrapper around the asynchronous
/// [`start_mining`](lb_api_service::http::pow::start_mining) function. Mining
/// is a fire-and-forget toggle that is not persisted, so a restart clears it.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// An [`OperationStatus`] error on failure, or [`OperationStatus::OK`] on
/// success.
pub(crate) fn pow_start_mining_sync(node: &LogosBlockchainNode) -> StatusResult<()> {
    node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::start_mining::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to start PoW mining: {error}"),
            )
        })
    })
}

/// Enables `PoW` mining.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// An [`OperationStatus`] error on failure, or [`OperationStatus::OK`] on
/// success.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pow_start_mining(node: *const LogosBlockchainNode) -> OperationStatus {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    unwrap_or_return_error!(pow_start_mining_sync(node));

    OperationStatus::OK
}

/// Disables `PoW` mining.
///
/// This is a synchronous wrapper around the asynchronous
/// [`stop_mining`](lb_api_service::http::pow::stop_mining) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// An [`OperationStatus`] error on failure, or [`OperationStatus::OK`] on
/// success.
pub(crate) fn pow_stop_mining_sync(node: &LogosBlockchainNode) -> StatusResult<()> {
    node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::stop_mining::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to stop PoW mining: {error}"),
            )
        })
    })
}

/// Disables `PoW` mining.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// An [`OperationStatus`] error on failure, or [`OperationStatus::OK`] on
/// success.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pow_stop_mining(node: *const LogosBlockchainNode) -> OperationStatus {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    unwrap_or_return_error!(pow_stop_mining_sync(node));

    OperationStatus::OK
}

/// Builds and publishes a reward-claim transaction for the currently claimable
/// tickets.
///
/// This is a synchronous wrapper around the asynchronous
/// [`claim`](lb_api_service::http::pow::claim) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`Result`] containing the submitted transaction hash on success. Returns
/// [`OperationStatusCode::NotFound`] when there are no rewards to claim, or
/// another [`OperationStatus`] error on failure.
pub(crate) fn pow_claim_sync(node: &LogosBlockchainNode) -> StatusResult<lb_core::mantle::TxHash> {
    let tx_hash = node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::claim::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Failed to claim PoW rewards: {error}"),
            )
        })
    })?;

    tx_hash.tx_hash.ok_or_else(|| {
        OperationStatus::error(
            OperationStatusCode::NotFound,
            "No PoW rewards available to claim.",
        )
    })
}

pub type FfiPoWClaimResult = FfiStatusResult<Hash>;

/// Builds and publishes a reward-claim transaction for the currently claimable
/// tickets, returning the submitted transaction hash.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiPoWClaimResult`] containing the submitted transaction hash on
/// success. The error is [`OperationStatusCode::NotFound`] when there are no
/// rewards to claim, or another [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pow_claim(node: *const LogosBlockchainNode) -> FfiPoWClaimResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let tx_hash = unwrap_or_return_error!(pow_claim_sync(node));

    let Ok(tx_hash_array): Result<Hash, _> =
        tx_hash.as_signing_bytes().iter().as_slice().try_into()
    else {
        return FfiPoWClaimResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "Failed to convert transaction hash to array.",
        ));
    };

    FfiPoWClaimResult::ok(tx_hash_array)
}

/// The rewards this node can currently claim.
#[repr(C)]
pub struct PoWClaimableRewards {
    /// Number of mined tickets still within the reward window.
    pub claimable_tickets: usize,
    /// For each claimable ticket, how many more slots it stays within the
    /// reward window before it can no longer be claimed. Points to `len`
    /// contiguous `u64` values.
    pub slots_until_expiry: *mut u64,
    /// Number of entries in `slots_until_expiry`.
    pub len: usize,
}

impl Default for PoWClaimableRewards {
    fn default() -> Self {
        Self {
            claimable_tickets: 0,
            slots_until_expiry: ptr::null_mut(),
            len: 0,
        }
    }
}

/// Reports the rewards this node can currently claim.
///
/// This is a synchronous wrapper around the asynchronous
/// [`claimable_rewards`](lb_api_service::http::pow::claimable_rewards)
/// function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`Result`] containing the claimable rewards info on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn pow_claimable_rewards_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<lb_pow_service::ClaimableRewardsInfo> {
    node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::claimable_rewards::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to get claimable PoW rewards: {error}"),
            )
        })
    })
}

pub type FfiPoWClaimableRewardsResult = FfiStatusResult<PoWClaimableRewards>;

/// Reports the rewards this node can currently claim.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiPoWClaimableRewardsResult`] containing the claimable rewards info on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance.
///
/// # Memory Management
///
/// This function allocates memory for the `slots_until_expiry` list. The caller
/// must free the returned value using the [`free_pow_claimable_rewards`]
/// function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pow_claimable_rewards(
    node: *const LogosBlockchainNode,
) -> FfiPoWClaimableRewardsResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let info = unwrap_or_return_error!(pow_claimable_rewards_sync(node));

    let slots: Vec<u64> = info.slots_until_expiry.into_iter().map(u64::from).collect();

    let len = slots.len();
    let slots_ptr = Box::leak(slots.into_boxed_slice()).as_mut_ptr();

    FfiPoWClaimableRewardsResult::ok(PoWClaimableRewards {
        claimable_tickets: info.claimable_tickets,
        slots_until_expiry: slots_ptr,
        len,
    })
}

/// Frees the memory allocated for a [`PoWClaimableRewards`] structure.
///
/// # Arguments
///
/// - `rewards`: A [`PoWClaimableRewards`] structure previously returned by
///   [`pow_claimable_rewards`].
///
/// # Safety
///
/// This function is unsafe because it reconstructs a boxed slice from a raw
/// pointer. The caller must only pass values returned by
/// [`pow_claimable_rewards`] and must call this exactly once per result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_pow_claimable_rewards(
    rewards: PoWClaimableRewards,
) -> OperationStatus {
    return_error_if_null_pointer!(rewards.slots_until_expiry);
    let slots = unsafe {
        Box::from_raw(ptr::slice_from_raw_parts_mut(
            rewards.slots_until_expiry,
            rewards.len,
        ))
    };

    drop(slots);
    OperationStatus::OK
}
