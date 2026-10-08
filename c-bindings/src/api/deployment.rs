use std::ffi::{CString, c_char};

use lb_c_macros::panic_to_error;
use lb_node::config::{DeploymentSettings, deployment::ProtocolScope};

use crate::{
    OperationStatus,
    api::{free, free_cstring, lifecycle::resolve_run_config},
    errors::{OperationStatusCode, free_operation_status},
    result::FfiStatusResult,
    return_error_if_null_pointer,
};

/// The libp2p protocol and topic names a deployment uses, derived from its
/// chain ID and fork digest.
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
        let deployment_chain_id = deployment.chain_id();
        let chain = ProtocolScope::Chain(&deployment_chain_id);
        let fork = ProtocolScope::Fork(deployment.fork_digest_in_force());

        // Every string is built before any of them is turned into a raw
        // pointer, so a failure part-way drops the ones already built instead
        // of leaking them.
        let chain_id = to_c_string(deployment_chain_id.as_ref())?;
        let blend = to_c_string(&fork.to_string_with_name("blend"))?;
        let cryptarchia = to_c_string(&fork.to_string_with_name("cryptarchia"))?;
        let kademlia = to_c_string(&chain.to_string_with_name("kad"))?;
        let identify = to_c_string(&chain.to_string_with_name("identify"))?;
        let chain_sync = to_c_string(&fork.to_string_with_name("chainsync"))?;
        let mempool = to_c_string(&fork.to_string_with_name("mempool"))?;
        let node_version = to_c_string(&lb_version::build_version_info().version)?;

        Ok(Self {
            chain_id: chain_id.into_raw(),
            genesis_time: deployment.genesis_time().unix_timestamp(),
            protocol_names: ProtocolNames {
                blend: blend.into_raw(),
                cryptarchia: cryptarchia.into_raw(),
                kademlia: kademlia.into_raw(),
                identify: identify.into_raw(),
                chain_sync: chain_sync.into_raw(),
                mempool: mempool.into_raw(),
            },
            node_version: node_version.into_raw(),
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
            // A null field has nothing to free; the status reporting it does.
            let status = unsafe { free_cstring(pointer) };
            unsafe { free_operation_status(status) };
        }
    }
}

fn to_c_string(value: &str) -> Result<CString, OperationStatus> {
    CString::new(value).map_err(|error| {
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
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_deployment_info(
    config_path: *const c_char,
    custom_deployment_path: *const c_char,
) -> FfiDeploymentInfoResult {
    return_error_if_null_pointer!(config_path);

    let run_config = match unsafe { resolve_run_config(config_path, custom_deployment_path) } {
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
/// - `pointer`: A pointer to the [`DeploymentInfo`] to be freed. A null pointer
///   frees nothing and returns a `NullPointer` error.
///
/// # Safety
///
/// A non-null pointer must come from [`get_deployment_info`] and must not have
/// been freed already.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_deployment_info(pointer: *mut DeploymentInfo) -> OperationStatus {
    return_error_if_null_pointer!(pointer);
    let deployment_info = unsafe { &mut *pointer };
    unsafe { deployment_info.free() };
    unsafe { free::<DeploymentInfo>(pointer) }
}
