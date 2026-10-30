use std::ffi::{CString, c_char};

use lb_blend_service::message::NetworkInfo;
use lb_c_macros::panic_to_error;
use lb_chain_service::Epoch;
use lb_core::{
    mantle::NoteId,
    sdp::{self, Locator, SNAPSHOT_FINALIZATION_DELAY, ServiceType},
};
use lb_groth16::fr_from_bytes;
use lb_node::{
    RuntimeServiceId, config::deployment::era::ruleset::EraRuleset,
    generic_services::blend::BlendService,
};
use lb_sdp_service::SdpSubmission;
use multiaddr::PeerId;

use crate::{
    LogosBlockchainNode,
    api::time::get_time_info_sync,
    errors::{OperationStatus, OperationStatusCode},
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
        lb_api_service::http::blend::blend_join_network::<
            BlendService<RuntimeServiceId>,
            RuntimeServiceId,
        >(node.get_overwatch_handle(), locator, service_note_id)
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
/// [`blend_info`](lb_api_service::http::blend::blend_info) function.
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
        .block_on(lb_api_service::http::blend::blend_info::<
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
        let withdrawal_unlock_delay_epochs =
            Epoch::new(SNAPSHOT_FINALIZATION_DELAY.into_inner() + 1).into();
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
