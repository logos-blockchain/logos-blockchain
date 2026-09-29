use std::ffi::{CString, c_char};

use lb_node::config::DeploymentSettings;

use crate::{
    OperationStatus,
    api::{free, free_cstring, lifecycle::resolve_run_config},
    errors::OperationStatusCode,
    result::FfiStatusResult,
    return_error_if_null_pointer,
};

/// The libp2p protocol and topic names a deployment uses.
#[repr(C)]
pub struct ProtocolNames {
    pub blend: *mut c_char,
    pub cryptarchia: *mut c_char,
    pub kademlia: *mut c_char,
    pub identify: *mut c_char,
    pub chain_sync: *mut c_char,
    pub mempool: *mut c_char,
}

/// What a configuration file says about the chain it points at.
#[repr(C)]
pub struct DeploymentInfo {
    pub chain_id: *mut c_char,
    pub genesis_time: u32,
    pub node_version: *mut c_char,
    pub protocol_names: ProtocolNames,
}

impl DeploymentInfo {
    fn new(deployment: &DeploymentSettings) -> Result<Self, OperationStatus> {
        Ok(Self {
            chain_id: into_c_string(deployment.chain_id().as_ref())?,
            genesis_time: deployment.genesis_time().unix_timestamp(),
            protocol_names: ProtocolNames {
                blend: into_c_string(deployment.blend.common.protocol_name.as_ref())?,
                cryptarchia: into_c_string(&deployment.cryptarchia.gossipsub_protocol)?,
                kademlia: into_c_string(deployment.network.kademlia_protocol_name.as_ref())?,
                identify: into_c_string(deployment.network.identify_protocol_name.as_ref())?,
                chain_sync: into_c_string(deployment.network.chain_sync_protocol_name.as_ref())?,
                mempool: into_c_string(&deployment.mempool.pubsub_topic)?,
            },
            node_version: into_c_string(&lb_version::build_version_info().version)?,
        })
    }

    unsafe fn free(&mut self) {
        for pointer in [
            self.chain_id,
            self.protocol_names.blend,
            self.protocol_names.cryptarchia,
            self.protocol_names.kademlia,
            self.protocol_names.identify,
            self.protocol_names.chain_sync,
            self.protocol_names.mempool,
            self.node_version,
        ] {
            unsafe { free_cstring(pointer) };
        }
    }
}

fn into_c_string(value: &str) -> Result<*mut c_char, OperationStatus> {
    CString::new(value).map(CString::into_raw).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )
    })
}

pub type FfiDeploymentInfoResult = FfiStatusResult<*mut DeploymentInfo>;

/// Describes the deployment a config points at, without starting a node.
///
/// # Arguments
///
/// - `config_path`: A non-null pointer to a string holding the path to the user
///   configuration file.
/// - `custom_deployment_path`: An optional pointer to a string holding the path
///   to a custom deployment file. If null, the `DEPLOYMENT` environment
///   variable is used, falling back to the embedded default deployment.
///
/// # Returns
///
/// A [`FfiDeploymentInfoResult`] containing a pointer to the allocated
/// [`DeploymentInfo`] struct on success, or an [`OperationStatus`] error on
/// failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that `config_path` is a valid NUL-terminated C
/// string, and that `custom_deployment_path` is either null or one as well.
///
/// # Memory Management
///
/// This function allocates the struct and every string it holds. The caller
/// must free all of it with [`free_deployment_info`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_deployment_info(
    config_path: *const c_char,
    custom_deployment_path: *const c_char,
) -> FfiDeploymentInfoResult {
    return_error_if_null_pointer!(config_path);

    let run_config = match resolve_run_config(config_path, custom_deployment_path) {
        Ok(run_config) => run_config,
        Err(error) => return FfiDeploymentInfoResult::err(error),
    };

    match DeploymentInfo::new(&run_config.deployment) {
        Ok(info) => FfiDeploymentInfoResult::from_value(info),
        Err(error) => FfiDeploymentInfoResult::err(error),
    }
}

/// Frees a [`DeploymentInfo`] and every string it owns.
///
/// # Arguments
///
/// - `pointer`: A pointer to the [`DeploymentInfo`] to be freed.
///
/// # Safety
///
/// The pointer must come from [`get_deployment_info`] and must not have been
/// freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_deployment_info(pointer: *mut DeploymentInfo) -> OperationStatus {
    return_error_if_null_pointer!(pointer);
    let deployment_info = unsafe { &mut *pointer };
    unsafe { deployment_info.free() };
    free::<DeploymentInfo>(pointer)
}
