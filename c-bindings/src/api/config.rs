use std::{
    ffi::{CStr, c_char},
    slice,
    str::FromStr as _,
};

use lb_node::config::InitArgs;
use multiaddr::Multiaddr;
use tokio::runtime::Runtime;

use crate::OperationStatus;

#[repr(C)]
pub enum DeploymentType {
    WellKnown = 0,
    Custom = 1,
}

#[repr(C)]
pub enum WellKnownDeployment {
    Devnet = 0,
}

#[repr(C)]
pub struct Deployment {
    pub deployment_type: DeploymentType,

    // Only valid if deployment_type is WellKnown.
    pub well_known_deployment: WellKnownDeployment,

    // Only valid if deployment_type is Custom.
    pub custom_deployment_config_path: *const c_char,
}

#[repr(C)]
pub struct GenerateConfigArgs {
    pub initial_peers: *const *const c_char,
    pub initial_peers_count: *const u32,
    pub output: *const c_char,
    pub net_port: *const u16,
    pub blend_port: *const u16,
    pub http_addr: *const c_char,
    pub external_address: *const c_char,
    pub no_public_ip_check: *const bool,
    pub deployment: *const Deployment,
    pub state_path: *const c_char,
}

impl From<GenerateConfigArgs> for InitArgs {
    fn from(value: GenerateConfigArgs) -> Self {
        let mut init_args = Self::default();

        // ---- initial_peers ----
        if !value.initial_peers.is_null() && !value.initial_peers_count.is_null() {
            let count = unsafe { *value.initial_peers_count } as usize;

            if count > 0 {
                let peers = unsafe { slice::from_raw_parts(value.initial_peers, count) };

                init_args.initial_peers = peers
                    .iter()
                    .filter_map(|&pointer| {
                        if pointer.is_null() {
                            return None;
                        }

                        unsafe { CStr::from_ptr(pointer) }
                            .to_str()
                            .ok()
                            .and_then(|string| Multiaddr::from_str(string).ok())
                    })
                    .collect();
            }
        }

        // ---- output ----
        if !value.output.is_null() {
            let output = unsafe { CStr::from_ptr(value.output) };
            init_args.output = output.to_string_lossy().to_string().into();
        }

        // ---- net_port ----
        if !value.net_port.is_null() {
            init_args.net_port = unsafe { *value.net_port };
        }

        // ---- blend_port ----
        if !value.blend_port.is_null() {
            init_args.blend_port = unsafe { *value.blend_port };
        }

        // ---- http_addr ----
        if !value.http_addr.is_null() {
            let http_address = unsafe { CStr::from_ptr(value.http_addr) };
            if let Ok(addr) = http_address.to_string_lossy().parse() {
                init_args.http_addr = addr;
            }
        }

        // ---- external_address ----
        if !value.external_address.is_null() {
            let external_address = unsafe { CStr::from_ptr(value.external_address) };
            init_args.external_address = external_address.to_string_lossy().parse().ok();
        }

        // ---- no_public_ip_check ----
        if !value.no_public_ip_check.is_null() {
            init_args.no_public_ip_check = unsafe { *value.no_public_ip_check };
        }

        // ---- deployment ----
        if !value.deployment.is_null() {
            let deployment = unsafe { &*value.deployment };

            match deployment.deployment_type {
                DeploymentType::WellKnown => {
                    init_args.deployment = match deployment.well_known_deployment {
                        WellKnownDeployment::Devnet => lb_node::config::DeploymentType::WellKnown(
                            lb_node::config::WellKnownDeployment::Devnet,
                        ),
                    };
                }
                DeploymentType::Custom => {
                    if !deployment.custom_deployment_config_path.is_null() {
                        let config_path =
                            unsafe { CStr::from_ptr(deployment.custom_deployment_config_path) };
                        let config_path = config_path.to_string_lossy().into_owned();

                        init_args.deployment =
                            lb_node::config::DeploymentType::Custom(config_path.into());
                    }
                }
            }
        }

        // ---- state_path ----
        if !value.state_path.is_null() {
            let state_path = unsafe { CStr::from_ptr(value.state_path) };
            init_args.state_path = Some(state_path.to_string_lossy().to_string().into());
        }

        init_args
    }
}

#[must_use]
pub fn generate_config_sync(args: InitArgs) -> OperationStatus {
    let runtime = Runtime::new().expect("Failed to create Tokio runtime.");
    let run_result = runtime.block_on(async move { lb_node::init::run(&args).await });
    match run_result {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            log::error!("Error generating config: {error:?}");
            OperationStatus::ConfigurationError
        }
    }
}

/// Generates the user config file.
///
/// # Arguments
///
/// * `args` - A [`GenerateConfigArgs`] struct containing the arguments to be
///   used for generating the config file.
///
/// # Returns
///
/// An `OperationStatus` indicating the result of the operation.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn generate_user_config(args: GenerateConfigArgs) -> OperationStatus {
    let init_args = InitArgs::from(args);
    generate_config_sync(init_args)
}
