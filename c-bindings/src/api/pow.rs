use std::ptr;

use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::{PoWService, RuntimeServiceId};
use lb_pow_service::AutoClaimTick;

use crate::{
    LogosBlockchainNode, OperationStatus,
    api::{cryptarchia::Hash, types::value::Value, wallet::parse_public_key},
    errors::OperationStatusCode,
    option::FfiOption,
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

/// Enables unattended `PoW` claiming.
///
/// This is a synchronous wrapper around the asynchronous
/// [`start_auto_claim`](lb_api_service::http::pow::start_auto_claim) function.
/// It has no effect when the node has no `auto_claim` targets configured, and
/// the service stops itself again once every target reaches its threshold.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// An [`OperationStatus`] error on failure, or [`OperationStatus::OK`] on
/// success.
pub(crate) fn pow_start_auto_claim_sync(node: &LogosBlockchainNode) -> StatusResult<()> {
    node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::start_auto_claim::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to start PoW auto-claim: {error}"),
            )
        })
    })
}

/// Enables unattended `PoW` claiming.
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
pub unsafe extern "C" fn pow_start_auto_claim(node: *const LogosBlockchainNode) -> OperationStatus {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    unwrap_or_return_error!(pow_start_auto_claim_sync(node));

    OperationStatus::OK
}

/// Disables unattended `PoW` claiming. Manual claims keep working.
///
/// This is a synchronous wrapper around the asynchronous
/// [`stop_auto_claim`](lb_api_service::http::pow::stop_auto_claim) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// An [`OperationStatus`] error on failure, or [`OperationStatus::OK`] on
/// success.
pub(crate) fn pow_stop_auto_claim_sync(node: &LogosBlockchainNode) -> StatusResult<()> {
    node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::stop_auto_claim::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to stop PoW auto-claim: {error}"),
            )
        })
    })
}

/// Disables unattended `PoW` claiming. Manual claims keep working.
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
pub unsafe extern "C" fn pow_stop_auto_claim(node: *const LogosBlockchainNode) -> OperationStatus {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    unwrap_or_return_error!(pow_stop_auto_claim_sync(node));

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
pub(crate) fn pow_claim_sync(
    node: &LogosBlockchainNode,
    claim_address: Option<ZkPublicKey>,
) -> StatusResult<lb_core::mantle::TxHash> {
    let tx_hash = node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::claim::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
            claim_address,
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
/// - `claim_address`: A pointer to the 32-byte little-endian public key the
///   rewards are paid to, or null to pay whichever auto-claim target is
///   currently furthest below its threshold.
///
/// # Returns
///
/// A [`FfiPoWClaimResult`] containing the submitted transaction hash on
/// success. The error is [`OperationStatusCode::NotFound`] when there are no
/// rewards to claim, or another [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance, and that `claim_address` is either null
/// or points to at least 32 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pow_claim(
    node: *const LogosBlockchainNode,
    claim_address: *const u8,
) -> FfiPoWClaimResult {
    return_error_if_null_pointer!(node);

    let claim_address = if claim_address.is_null() {
        None
    } else {
        Some(unwrap_or_return_error!(unsafe {
            parse_public_key(claim_address)
        }))
    };

    let node = unsafe { &*node };
    let tx_hash = unwrap_or_return_error!(pow_claim_sync(node, claim_address));

    FfiPoWClaimResult::ok(tx_hash.0)
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

#[repr(C)]
pub enum PoWAutoClaimTickUnit {
    Seconds,
    Slots,
}

#[repr(C)]
pub struct PoWClaimTargetStatus {
    /// The target's public key, as 32 little-endian bytes.
    pub public_key: [u8; 32],
    pub threshold: Value,
    /// `None` means the wallet couldn't be read.
    pub balance: FfiOption<Value>,
}

/// The runtime state of unattended claiming.
///
/// Mirrors [`lb_pow_service::AutoClaimStatus`], except for the tick:
/// [`AutoClaimTick`] keeps its period inside the variant, which C cannot
/// express, so it arrives here as a `tick` plus the `tick_unit` that reads it.
#[repr(C)]
pub struct PoWAutoClaimStatus {
    pub is_armed: bool,
    pub tick: u64,
    pub tick_unit: PoWAutoClaimTickUnit,
    /// The configured claim targets. Points to `targets_len` contiguous
    /// [`PoWClaimTargetStatus`] values.
    pub targets: *mut PoWClaimTargetStatus,
    /// Number of entries in `targets`.
    pub targets_len: usize,
}

impl Default for PoWAutoClaimStatus {
    fn default() -> Self {
        Self {
            is_armed: false,
            tick: 0,
            tick_unit: PoWAutoClaimTickUnit::Seconds,
            targets: ptr::null_mut(),
            targets_len: 0,
        }
    }
}

/// The runtime state of the `PoW` service, as the running service holds it.
#[repr(C)]
#[derive(Default)]
pub struct PoWStatus {
    pub is_mining: bool,
    pub are_rewards_enabled: bool,
    pub auto_claim: PoWAutoClaimStatus,
}

/// Reports the runtime state of the `PoW` service.
///
/// This is a synchronous wrapper around the asynchronous
/// [`status`](lb_api_service::http::pow::status) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`Result`] containing the service status on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn pow_status_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<lb_pow_service::PoWStatus> {
    node.get_runtime_handle().block_on(async {
        lb_api_service::http::pow::status::<PoWService, RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to get PoW status: {error}"),
            )
        })
    })
}

pub type FfiPoWStatusResult = FfiStatusResult<PoWStatus>;

/// Reports the runtime state of the `PoW` service.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiPoWStatusResult`] containing the service status on success, or an
/// [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer.
/// The caller must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance.
///
/// # Memory Management
///
/// This function allocates memory for the `auto_claim.targets` list.
/// The caller must free the returned value using the [`free_pow_status`]
/// function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pow_status(node: *const LogosBlockchainNode) -> FfiPoWStatusResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let status = unwrap_or_return_error!(pow_status_sync(node));

    let (tick, tick_unit) = match status.auto_claim.tick {
        AutoClaimTick::Seconds(seconds) => (seconds.get(), PoWAutoClaimTickUnit::Seconds),
        AutoClaimTick::Slots(slots) => (slots.get(), PoWAutoClaimTickUnit::Slots),
    };

    let targets: Vec<PoWClaimTargetStatus> = status
        .auto_claim
        .targets
        .into_iter()
        .map(|target| PoWClaimTargetStatus {
            public_key: fr_to_bytes(target.public_key.as_fr()),
            threshold: target.threshold,
            balance: target.balance.into(),
        })
        .collect();

    let len = targets.len();
    let targets_ptr = Box::leak(targets.into_boxed_slice()).as_mut_ptr();

    FfiPoWStatusResult::ok(PoWStatus {
        is_mining: status.is_mining,
        are_rewards_enabled: status.are_rewards_enabled,
        auto_claim: PoWAutoClaimStatus {
            is_armed: status.auto_claim.is_armed,
            tick,
            tick_unit,
            targets: targets_ptr,
            targets_len: len,
        },
    })
}

/// Frees the memory allocated for a [`PoWStatus`] structure.
///
/// # Arguments
///
/// - `status`: A [`PoWStatus`] structure previously returned by [`pow_status`].
///
/// # Safety
///
/// This function is unsafe because it reconstructs a boxed slice from a raw
/// pointer.
/// The caller must only pass values returned by [`pow_status`] and must call
/// this exactly once per result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_pow_status(status: PoWStatus) -> OperationStatus {
    // A null list means nothing was allocated — as after an error — so there is
    // nothing to free and the caller did nothing wrong.
    if status.auto_claim.targets.is_null() {
        return OperationStatus::OK;
    }
    let targets = unsafe {
        Box::from_raw(ptr::slice_from_raw_parts_mut(
            status.auto_claim.targets,
            status.auto_claim.targets_len,
        ))
    };

    drop(targets);
    OperationStatus::OK
}
