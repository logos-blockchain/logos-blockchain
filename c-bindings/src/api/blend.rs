use std::ffi::{CString, c_char};

use lb_api_service::http::blend;
use lb_blend_service::message::NetworkInfo;
use lb_c_macros::panic_to_error;
use lb_chain_service::Epoch;
use lb_core::{
    mantle::NoteId,
    sdp::{self, Locator, SNAPSHOT_FINALIZATION_DELAY, ServiceType, WITHDRAWAL_UNLOCK_DELAY},
};
use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_ledger::mantle::sdp::rewards::blend as ledger_blend;
use lb_node::{
    RuntimeServiceId, config::deployment::era::ruleset::EraRuleset,
    generic_services::blend::BlendService,
};
use lb_sdp_service::{SdpMessage, SdpStatus, SdpSubmission, status};
use multiaddr::PeerId;
use overwatch::overwatch::OverwatchHandle;

use crate::{
    LogosBlockchainNode,
    api::{
        chain::api::ChainFfiApi, common::overwatch::ensure_ready, cryptarchia::TxHash,
        sdp::make_sdp_request, time::get_time_info_sync, types::value::Value,
    },
    errors::{OperationStatus, OperationStatusCode},
    option::FfiOption,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

pub const KEY_SIZE: usize = 32;

#[repr(C)]
#[derive(Default)]
pub struct DeclarationId(pub [u8; KEY_SIZE]);

impl From<sdp::DeclarationId> for DeclarationId {
    fn from(id: sdp::DeclarationId) -> Self {
        Self(id.0)
    }
}

unsafe fn parse_service_note_id(ptr: *const u8) -> Result<NoteId, OperationStatus> {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, KEY_SIZE) };
    fr_from_bytes(bytes).map(NoteId).map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "Invalid `service_note_id` bytes.",
        )
    })
}

unsafe fn parse_locator(ptr: *const c_char) -> Result<Locator, OperationStatus> {
    let c_str = unsafe { std::ffi::CStr::from_ptr(ptr) };
    let Ok(s) = c_str.to_str() else {
        return Err(OperationStatus::error(
            OperationStatusCode::ValidationError,
            "`locator` is not valid UTF-8.",
        ));
    };
    s.parse::<Locator>().map_err(|_| {
        OperationStatus::error(
            OperationStatusCode::ValidationError,
            "`locator` is not a valid locator.",
        )
    })
}

/// Joins the Blend network as a core node.
///
/// This delegates to the Blend service, which derives the node's own
/// `provider_id` and `zk_id` internally, mirroring the `POST /blend/join` HTTP
/// endpoint.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `locator`: A non-null pointer to a locator C string.
/// - `service_note_id`: A non-null pointer to 32 bytes representing the service
///   note ID.
///
/// # Returns
///
/// A [`FfiStatusResult`] containing the declaration ID as a 32-byte array on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_join_as_core_node(
    node: *const LogosBlockchainNode,
    locator: *const c_char,
    service_note_id: *const u8,
) -> FfiStatusResult<DeclarationId> {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(locator);
    return_error_if_null_pointer!(service_note_id);

    let locator = unwrap_or_return_error!(unsafe { parse_locator(locator) });
    let service_note_id =
        unwrap_or_return_error!(unsafe { parse_service_note_id(service_note_id) });

    let node = unsafe { &*node };

    let runtime_handle = unwrap_or_return_error!(node.get_runtime_handle());
    let result: StatusResult<SdpSubmission> = runtime_handle.block_on(async {
        blend::blend_join_network::<BlendService<RuntimeServiceId>, RuntimeServiceId>(
            node.get_overwatch_handle(),
            locator,
            service_note_id,
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to join blend network: {error}"),
            )
        })
    });

    result
        .map(|submission| DeclarationId::from(submission.declaration_id))
        .into()
}

/// Gets the current Blend network information.
///
/// This is a synchronous wrapper around the asynchronous
/// [`blend_info`](blend::blend_info) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A `Result` containing the Blend network information on success, or an
/// [`OperationStatus`] error on failure. The value is `None` when the node has
/// not joined the Blend network as a core node.
pub(crate) fn blend_info_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<Option<NetworkInfo<PeerId>>> {
    let runtime_handle = node.get_runtime_handle()?;
    let overwatch_handle = node.get_overwatch_handle();

    runtime_handle
        .block_on(blend::blend_info::<
            BlendService<RuntimeServiceId>,
            RuntimeServiceId,
        >(overwatch_handle))
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to get blend info: {error}"),
            )
        })
}

pub type FfiBlendInfoResult = FfiStatusResult<*mut c_char>;

/// Gets the current Blend network information as a JSON string.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiBlendInfoResult`] containing a pointer to an allocated C string (the
/// JSON-serialized network information) on success, or an [`OperationStatus`]
/// error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_info(node: *const LogosBlockchainNode) -> FfiBlendInfoResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let network_info = unwrap_or_return_error!(blend_info_sync(node));

    let json_network_info =
        unwrap_or_return_error!(serde_json::to_string(&network_info).map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to serialize blend info: {error}"),
            )
        }));
    let json_network_info_c =
        unwrap_or_return_error!(CString::new(json_network_info).map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to create CString: {error}"),
            )
        }));
    FfiBlendInfoResult::ok(json_network_info_c.into_raw())
}

/// What a node needs to take part in Blend.
#[repr(C)]
#[derive(Default)]
pub struct BlendRequirements {
    pub minimum_stake: u64,
    pub declaration_activation_delay_epochs: u32,
    pub withdrawal_unlock_delay_epochs: u32,
    pub inactivity_timeout_epochs: u32,
    pub minimum_network_size_for_rewards: u64,
}

impl BlendRequirements {
    fn new(era: &EraRuleset) -> Result<Self, OperationStatus> {
        let inactivity_period = era
            .inactivity_period(ServiceType::BlendNetwork)
            .ok_or_else(|| {
                OperationStatus::error(
                    OperationStatusCode::ConfigurationError,
                    "The deployment has no Blend service parameters.",
                )
            })?;

        let minimum_stake = era.min_stake().threshold;
        let declaration_activation_delay_epochs = SNAPSHOT_FINALIZATION_DELAY.into();
        let withdrawal_unlock_delay_epochs = WITHDRAWAL_UNLOCK_DELAY.into();
        let inactivity_timeout_epochs = inactivity_period.into_inner().into();
        let minimum_network_size_for_rewards = era.blend_reward_params().minimum_network_size.get();

        Ok(Self {
            minimum_stake,
            declaration_activation_delay_epochs,
            withdrawal_unlock_delay_epochs,
            inactivity_timeout_epochs,
            minimum_network_size_for_rewards,
        })
    }
}

/// Gets what a node needs to take part in Blend, under the active era at the
/// current epoch.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// The requirements on success, or an [`OperationStatus`] error on failure.
fn blend_requirements_sync(node: &LogosBlockchainNode) -> StatusResult<BlendRequirements> {
    let current_epoch = Epoch::new(get_time_info_sync(node)?.current_epoch);
    let era = &node
        .deployment()
        .era_schedule()
        .at_epoch(current_epoch)
        .parameters
        .ruleset;
    BlendRequirements::new(era)
}

pub type FfiBlendRequirementsResult = FfiStatusResult<BlendRequirements>;

/// Gets what a node needs to take part in Blend, under the active era at the
/// current epoch.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiBlendRequirementsResult`] containing the [`BlendRequirements`] on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_requirements(
    node: *const LogosBlockchainNode,
) -> FfiBlendRequirementsResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    blend_requirements_sync(node).into()
}

#[repr(C)]
#[derive(Default)]
pub struct BlendWithdrawal {
    pub tx_hash: TxHash,
    pub unlocks_at_epoch: u32,
}

async fn post_withdraw(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
) -> StatusResult<SdpSubmission> {
    let SdpStatus { participation, .. } = make_sdp_request(overwatch_handle, |reply_channel| {
        SdpMessage::GetStatus { reply_channel }
    })
    .await?;

    let live_declaration_id = participation
        .and_then(|participation| participation.live())
        .ok_or_else(|| {
            OperationStatus::error(
                OperationStatusCode::NotFound,
                "The node has no live Blend declaration to withdraw.",
            )
        })?;

    make_sdp_request(overwatch_handle, |reply_channel| {
        SdpMessage::PostWithdrawal {
            declaration_id: live_declaration_id,
            reply_channel,
        }
    })
    .await?
    .map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::ServiceError,
            format!("Failed to withdraw: {error}"),
        )
    })
}

/// Withdraws this node's Blend declaration, leaving the core set.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// The withdrawal on success, or an [`OperationStatus`] error on failure.
/// Fails with [`OperationStatusCode::NotFound`] if the node has no live
/// declaration, and right away if the node is not `Online`.
fn blend_withdraw_sync(node: &LogosBlockchainNode) -> StatusResult<BlendWithdrawal> {
    let overwatch_handle = node.get_overwatch_handle();
    let runtime_handle = node.get_runtime_handle()?;
    runtime_handle.block_on(async {
        let current_epoch = ChainFfiApi::from_overwatch_handle(overwatch_handle)
            .await?
            .get_online_epoch()
            .await?;

        let withdraw_tx_hash = {
            let SdpSubmission { tx_id, .. } = post_withdraw(overwatch_handle).await?;

            let Some(tx_id) = tx_id else {
                return Err(OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    "The withdrawal was not submitted.",
                ));
            };

            tx_id
        };

        let unlocks_at_epoch = current_epoch.strict_add(WITHDRAWAL_UNLOCK_DELAY).into();
        Ok(BlendWithdrawal {
            tx_hash: withdraw_tx_hash.0,
            unlocks_at_epoch,
        })
    })
}

/// Withdraws this node's Blend declaration, leaving the core set.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiStatusResult`] containing the [`BlendWithdrawal`] on success, or an
/// [`OperationStatus`] error on failure.
/// Fails right away if the node is not `Online`.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_withdraw(
    node: *const LogosBlockchainNode,
) -> FfiStatusResult<BlendWithdrawal> {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    blend_withdraw_sync(node).into()
}

/// The activity epoch's network.
#[repr(C)]
#[derive(Default)]
pub struct BlendNetwork {
    /// Number of core nodes in the activity epoch.
    pub size: u64,
    /// Number of activity proofs submitted.
    pub proofs: u64,
}

impl From<ledger_blend::Network> for BlendNetwork {
    fn from(value: ledger_blend::Network) -> Self {
        Self {
            size: value.size,
            proofs: value.proofs,
        }
    }
}

/// A node's estimated Blend reward for the activity epoch, based on the proofs
/// submitted so far.
#[repr(C)]
#[derive(Default)]
pub struct BlendRewardEstimate {
    /// The epoch the reward is for, when the blending was done.
    pub activity_epoch: u32,
    /// Blend income to be split.
    pub pool: Value,
    /// The activity epoch's network.
    pub network: BlendNetwork,
    /// Whether the node was in the activity epoch, and so can earn from it.
    pub in_activity_epoch: bool,
    /// Whether the node submitted its activity proof.
    pub did_submit_proof: bool,
    /// Whether the node earns the double reward.
    pub is_premium: bool,
    /// What the node would be paid.
    pub estimated_reward: Value,
}

impl From<ledger_blend::RewardEstimate> for BlendRewardEstimate {
    fn from(estimate: ledger_blend::RewardEstimate) -> Self {
        Self {
            activity_epoch: estimate.activity_epoch.into(),
            pool: estimate.pool,
            network: estimate.network.into(),
            in_activity_epoch: estimate.in_activity_epoch,
            did_submit_proof: estimate.did_submit_proof,
            is_premium: estimate.is_premium,
            estimated_reward: estimate.estimated_reward,
        }
    }
}

/// A node's Blend rewards.
#[repr(C)]
#[derive(Default)]
pub struct BlendRewards {
    /// The declaration's `zk_id`, as 32 little-endian bytes. Rewards are minted
    /// to it.
    pub zk_id: [u8; KEY_SIZE],
    /// `None` while no epoch is being paid out.
    pub estimate: FfiOption<BlendRewardEstimate>,
}

impl From<lb_chain_service::BlendRewards> for BlendRewards {
    fn from(rewards: lb_chain_service::BlendRewards) -> Self {
        Self {
            zk_id: fr_to_bytes(rewards.zk_id.as_fr()),
            estimate: rewards.estimate.map(Into::into).into(),
        }
    }
}

/// Estimates this node's Blend reward for the epoch being paid out.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// The rewards on success, or an [`OperationStatus`] error on failure. Fails
/// with [`OperationStatusCode::NotFound`] if the node has no declaration in
/// the ledger, and right away if the node is not `Online`.
fn blend_rewards_sync(node: &LogosBlockchainNode) -> StatusResult<BlendRewards> {
    let overwatch_handle = node.get_overwatch_handle();
    node.get_runtime_handle()?.block_on(async {
        let chain_ffi_api = ChainFfiApi::from_overwatch_handle(overwatch_handle).await?;
        chain_ffi_api.ensure_online().await?;
        let SdpStatus { participation, .. } = make_sdp_request(overwatch_handle, |reply_channel| {
            SdpMessage::GetStatus { reply_channel }
        })
        .await?;
        // A withdrawn declaration still earns until its `withdraw_at`.
        let Some(participation) = participation else {
            return Err(OperationStatus::error(
                OperationStatusCode::NotFound,
                "The node has no Blend declaration.",
            ));
        };

        let rewards = chain_ffi_api
            .get_blend_rewards(participation.declaration_id())
            .await?;
        Ok(rewards.into())
    })
}

pub type FfiBlendRewardsResult = FfiStatusResult<BlendRewards>;

/// Estimates this node's Blend reward for the epoch being paid out.
///
/// Blend rewards are minted to the declaration's `zk_id` one epoch later, with
/// no claim needed. This is the estimate before that happens, based on the
/// proofs submitted so far. The settled amount is in the
/// `SdpRewardDistributed` block event.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiBlendRewardsResult`] containing the [`BlendRewards`] on success, or
/// an [`OperationStatus`] error on failure. Fails right away if the node is not
/// `Online`.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_rewards(node: *const LogosBlockchainNode) -> FfiBlendRewardsResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    blend_rewards_sync(node).into()
}

/// Where the node's Blend declaration is in its lifecycle.
///
/// cbindgen:prefix-with-name
#[repr(C)]
#[derive(Default)]
pub enum DeclarationState {
    /// No declaration posted.
    #[default]
    NotDeclared,
    /// Posted, but not in the ledger yet.
    Pending,
    /// In the ledger, but not in an epoch snapshot yet.
    Activating,
    /// In the current epoch's snapshot.
    Active,
    /// Past `first_epoch_in_snapshot`, but not in the current epoch's snapshot.
    Inactive,
    /// Withdrawal posted. Once it lands, the declaration is withdrawn at
    /// `withdraw_at`, and the locked note unlocks the epoch after.
    Withdrawn,
}

impl From<status::DeclarationState> for DeclarationState {
    fn from(state: status::DeclarationState) -> Self {
        match state {
            status::DeclarationState::NotDeclared => Self::NotDeclared,
            status::DeclarationState::Pending => Self::Pending,
            status::DeclarationState::Activating => Self::Activating,
            status::DeclarationState::Active => Self::Active,
            status::DeclarationState::Inactive => Self::Inactive,
            status::DeclarationState::Withdrawn => Self::Withdrawn,
        }
    }
}

/// The kind of [`ProofState`].
///
/// cbindgen:prefix-with-name
#[repr(C)]
#[derive(Default)]
pub enum ProofStateKind {
    /// Submitted, but not in the ledger yet.
    #[default]
    Pending,
    /// In the ledger.
    Accepted,
    /// Not in the ledger, and not pending.
    Failed,
    /// No proof owed.
    NotRequired,
}

/// Why a [`ProofStateKind`] is `Failed` or `NotRequired`.
#[repr(C)]
#[derive(Default)]
pub enum ProofReason {
    /// `Failed`: no proof submitted. Also seen briefly at the start of an
    /// epoch, before the proof is submitted.
    #[default]
    NoProof,
    /// `Failed`: the node's declaration wasn't found when submitting the
    /// proof.
    DeclarationNotFound,
    /// `Failed`: the proof transaction couldn't be built.
    TransactionBuildFailed,
    /// `Failed`: the mempool rejected the proof transaction.
    MempoolRejected,
    /// `NotRequired`: the chain is in its first epoch, so there's no activity
    /// epoch.
    FirstEpoch,
    /// `NotRequired`: the activity epoch had fewer providers than the minimum
    /// network size, so the ledger accepts no proofs for it.
    NetworkBelowMinimum,
    /// `NotRequired`: the node wasn't a provider in the activity epoch, as in
    /// its first active epoch.
    NotInActivityEpoch,
}

impl From<status::ProofFailureReason> for ProofReason {
    fn from(reason: status::ProofFailureReason) -> Self {
        match reason {
            status::ProofFailureReason::NoProof => Self::NoProof,
            status::ProofFailureReason::DeclarationNotFound => Self::DeclarationNotFound,
            status::ProofFailureReason::TransactionBuildFailed => Self::TransactionBuildFailed,
            status::ProofFailureReason::MempoolRejected => Self::MempoolRejected,
        }
    }
}

impl From<status::ProofNotRequiredReason> for ProofReason {
    fn from(reason: status::ProofNotRequiredReason) -> Self {
        match reason {
            status::ProofNotRequiredReason::FirstEpoch => Self::FirstEpoch,
            status::ProofNotRequiredReason::NetworkBelowMinimum => Self::NetworkBelowMinimum,
            status::ProofNotRequiredReason::NotInActivityEpoch => Self::NotInActivityEpoch,
        }
    }
}

/// The state of the node's activity proof for the activity epoch.
#[repr(C)]
#[derive(Default)]
pub struct ProofState {
    pub kind: ProofStateKind,
    /// Set when `kind` is `Failed` or `NotRequired`.
    pub reason: FfiOption<ProofReason>,
}

impl From<status::ProofState> for ProofState {
    fn from(state: status::ProofState) -> Self {
        let (kind, reason) = match state {
            status::ProofState::Pending => (ProofStateKind::Pending, None),
            status::ProofState::Accepted => (ProofStateKind::Accepted, None),
            status::ProofState::Failed(reason) => (ProofStateKind::Failed, Some(reason.into())),
            status::ProofState::NotRequired(reason) => {
                (ProofStateKind::NotRequired, Some(reason.into()))
            }
        };
        Self {
            kind,
            reason: reason.into(),
        }
    }
}

/// What [`DeclarationStatus`] exposes of a [`sdp::Declaration`].
#[repr(C)]
#[derive(Default)]
pub struct Declaration {
    /// As 32 bytes.
    pub provider_id: [u8; KEY_SIZE],
    /// The note locked by the declaration, as 32 little-endian bytes.
    pub locked_note_id: [u8; KEY_SIZE],
    /// As 32 little-endian bytes.
    pub zk_id: [u8; KEY_SIZE],
    /// The epoch of the block that contained the declaration.
    pub created_epoch: u32,
    /// The epoch at which the declaration is scheduled to be withdrawn. `None`
    /// until the withdrawal lands.
    pub withdraw_at: FfiOption<u32>,
}

impl From<sdp::Declaration> for Declaration {
    fn from(declaration: sdp::Declaration) -> Self {
        Self {
            provider_id: *declaration.provider_id.as_ref(),
            locked_note_id: fr_to_bytes(declaration.service_note_id.as_fr()),
            zk_id: fr_to_bytes(declaration.zk_id.as_fr()),
            created_epoch: declaration.created.into(),
            withdraw_at: declaration.withdraw_at.map(Into::into).into(),
        }
    }
}

/// The node's Blend declaration in the ledger.
#[repr(C)]
#[derive(Default)]
pub struct DeclarationStatus {
    pub id: DeclarationId,
    pub declaration: Declaration,
    /// The first epoch whose snapshot includes the declaration.
    pub first_epoch_in_snapshot: u32,
}

impl From<status::DeclarationStatus> for DeclarationStatus {
    fn from(status: status::DeclarationStatus) -> Self {
        Self {
            id: status.id.into(),
            declaration: status.declaration.into(),
            first_epoch_in_snapshot: status.first_epoch_in_snapshot.into(),
        }
    }
}

/// The node's Blend activity.
#[repr(C)]
#[derive(Default)]
pub struct ActivityStatus {
    /// The epoch being paid out. `None` when there's no activity epoch.
    pub activity_epoch: FfiOption<u32>,
    /// The state of the activity proof for `activity_epoch`.
    pub proof_state: ProofState,
    /// The epoch in which the node's latest activity proof landed. Starts at
    /// the declaration's `first_epoch_in_snapshot`.
    pub last_active_epoch: u32,
}

impl From<status::ActivityStatus> for ActivityStatus {
    fn from(status: status::ActivityStatus) -> Self {
        Self {
            activity_epoch: status.activity_epoch.map(Into::into).into(),
            proof_state: status.proof_state.into(),
            last_active_epoch: status.last_active_epoch.into(),
        }
    }
}

/// The negotiated peers for the current epoch.
#[repr(C)]
#[derive(Default)]
pub struct SessionPeers {
    pub total: usize,
    pub healthy: usize,
}

impl From<lb_blend_service::message::SessionPeers> for SessionPeers {
    fn from(peers: lb_blend_service::message::SessionPeers) -> Self {
        Self {
            total: peers.total,
            healthy: peers.healthy,
        }
    }
}

/// A core node's Blend status.
#[repr(C)]
#[derive(Default)]
pub struct CoreStatus {
    pub session_peers: SessionPeers,
    /// Blending tokens collected in the current epoch.
    pub tokens_collected_in_current_epoch: usize,
}

impl From<lb_blend_service::message::CoreStatus> for CoreStatus {
    fn from(status: lb_blend_service::message::CoreStatus) -> Self {
        Self {
            session_peers: status.session_peers.into(),
            tokens_collected_in_current_epoch: status.tokens_collected_in_current_epoch,
        }
    }
}

/// The node's Blend declaration and activity, as seen by SDP.
#[repr(C)]
#[derive(Default)]
pub struct SdpBlendStatus {
    pub declaration_state: DeclarationState,
    /// The epoch of the chain tip.
    pub current_epoch: u32,
    /// `None` while the declaration isn't in the ledger.
    pub declaration: FfiOption<DeclarationStatus>,
    /// `None` while the declaration isn't in the ledger, or while it's
    /// activating.
    pub activity: FfiOption<ActivityStatus>,
}

impl From<status::SdpBlendStatus> for SdpBlendStatus {
    fn from(status: status::SdpBlendStatus) -> Self {
        Self {
            declaration_state: status.declaration_state.into(),
            current_epoch: status.current_epoch.into(),
            declaration: status.declaration.map(Into::into).into(),
            activity: status.activity.map(Into::into).into(),
        }
    }
}

/// The node's Blend status.
#[repr(C)]
#[derive(Default)]
pub struct BlendStatus {
    pub sdp: SdpBlendStatus,
    /// `None` when not a core node.
    pub core: FfiOption<CoreStatus>,
}

/// Gets this node's Blend status: core or edge, its declaration and its
/// activity proof for the activity epoch.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// The status on success, or an [`OperationStatus`] error on failure. Fails
/// right away if the node is not `Online`.
fn blend_status_sync(node: &LogosBlockchainNode) -> StatusResult<BlendStatus> {
    let overwatch_handle = node.get_overwatch_handle();
    node.get_runtime_handle()?.block_on(async {
        ensure_ready::<BlendService<RuntimeServiceId>>(overwatch_handle).await?;

        let chain_ffi_api = ChainFfiApi::from_overwatch_handle(overwatch_handle).await?;
        chain_ffi_api.ensure_online().await?;

        let status = make_sdp_request(overwatch_handle, |reply_channel| {
            SdpMessage::GetBlendStatus { reply_channel }
        })
        .await?
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Failed to get the Blend status: {error}"),
            )
        })?;

        let core_status = blend::blend_core_status::<
            BlendService<RuntimeServiceId>,
            RuntimeServiceId,
        >(overwatch_handle)
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to query the Blend service: {error}"),
            )
        })?;

        Ok(BlendStatus {
            sdp: status.into(),
            core: core_status.map(Into::into).into(),
        })
    })
}

pub type FfiBlendStatusResult = FfiStatusResult<BlendStatus>;

/// Gets this node's Blend status: core or edge, its declaration and its
/// activity proof for the activity epoch.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiBlendStatusResult`] containing the [`BlendStatus`] on success, or an
/// [`OperationStatus`] error on failure.
/// Fails right away if the node is not `Online`.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blend_status(node: *const LogosBlockchainNode) -> FfiBlendStatusResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    blend_status_sync(node).into()
}
