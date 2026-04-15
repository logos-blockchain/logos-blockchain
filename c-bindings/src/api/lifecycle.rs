use std::{ffi::c_char, path::PathBuf};

use lb_node::{
    UserConfig,
    config::{
        DeploymentType, OnUnknownKeys, RunConfig,
        deployment::{DeploymentSettings, WellKnownDeployment},
        deserialize_config_at_path,
    },
    get_services_to_start, run_node_from_config,
};
use tokio::runtime::Runtime;

use crate::{LogosBlockchainNode, PointerResult, errors::OperationStatus};

pub type InitializedLogosBlockchainNodeResult = PointerResult<LogosBlockchainNode, OperationStatus>;

/// Creates and starts a Logos blockchain node based on the provided
/// configuration file path.
///
/// # Arguments
///
/// - `config_path`: A pointer to a string representing the path to the
///   configuration file.
/// - `deployment`: A pointer to a string representing either a well-known
///   deployment name (e.g., "devnet") or a path to a deployment YAML file. If
///   null, defaults to "devnet".
///
/// # Returns
///
/// An `InitializedLogosBlockchainNodeResult` containing either a pointer to the
/// initialized `LogosBlockchainNode` or an error code.
#[unsafe(no_mangle)]
pub extern "C" fn start_lb_node(
    config_path: *const c_char,
    deployment: *const c_char,
) -> InitializedLogosBlockchainNodeResult {
    initialize_lb_node(config_path, deployment).map_or_else(
        InitializedLogosBlockchainNodeResult::from_error,
        InitializedLogosBlockchainNodeResult::from_value,
    )
}

/// Initializes and starts a Logos blockchain node based on the provided
/// configuration file path.
///
/// # Arguments
///
/// - `config_path`: A pointer to a string representing the path to the
///   configuration file.
/// - `deployment`: A pointer to a string representing either a well-known
///   deployment name (e.g., "devnet") or a path to a deployment YAML file. If
///   null, defaults to "devnet".
///
/// # Returns
///
/// A `Result` containing either the initialized `LogosBlockchainNode` or an
/// error code.
fn initialize_lb_node(
    config_path: *const c_char,
    deployment: *const c_char,
) -> Result<LogosBlockchainNode, OperationStatus> {
    let run_config = RunConfig {
        deployment: get_deployment_config(deployment)?,
        user: get_user_config(config_path)?,
    };

    let rt = Runtime::new().unwrap();
    let app = run_node_from_config(run_config).map_err(|e| {
        log::error!("Could not initialize Overwatch: {e}");
        OperationStatus::InitializationError
    })?;

    let app_handle = app.handle();

    rt.block_on(async {
        let services_to_start = get_services_to_start(&app).await.map_err(|e| {
            log::error!("Could not get services to start: {e}");
            OperationStatus::InitializationError
        })?;
        app_handle
            .start_service_sequence(services_to_start)
            .await
            .map_err(|e| {
                log::error!("Could not start services: {e}");
                OperationStatus::InitializationError
            })?;
        Ok(())
    })?;

    Ok(LogosBlockchainNode::new(app, rt))
}

fn get_user_config(config_path: *const c_char) -> Result<UserConfig, OperationStatus> {
    let user_config_path = unsafe { std::ffi::CStr::from_ptr(config_path) }
        .to_str()
        .map_err(|e| {
            log::error!("Could not convert the config path to string: {e}");
            OperationStatus::InitializationError
        })?;
    deserialize_config_at_path::<UserConfig>(user_config_path.as_ref(), OnUnknownKeys::Warn)
        .map_err(|e| {
            log::error!("Could not parse config file: {e}");
            OperationStatus::InitializationError
        })
}

fn get_deployment_config(
    deployment_arg: *const c_char,
) -> Result<DeploymentSettings, OperationStatus> {
    let deployment_type: DeploymentType = if deployment_arg.is_null() {
        WellKnownDeployment::default().into()
    } else {
        let deployment_str = unsafe { std::ffi::CStr::from_ptr(deployment_arg) }
            .to_str()
            .map_err(|e| {
                log::error!("Could not convert deployment to string: {e}");
                OperationStatus::InitializationError
            })?;
        deployment_str.parse::<WellKnownDeployment>().map_or_else(
            |()| PathBuf::from(deployment_str).into(),
            DeploymentType::from,
        )
    };

    match deployment_type {
        DeploymentType::WellKnown(well_known_deployment) => Ok(well_known_deployment.into()),
        DeploymentType::Custom(path) => {
            deserialize_config_at_path::<DeploymentSettings>(path.as_ref(), OnUnknownKeys::Warn)
                .map_err(|e| {
                    log::error!("Could not parse deployment file: {e}");
                    OperationStatus::InitializationError
                })
        }
    }
}

/// Stops and frees the resources associated with the given Logos blockchain
/// node.
///
/// # Arguments
///
/// - `node`: A pointer to the `LogosBlockchainNode` instance to be stopped.
///
/// # Returns
///
/// An `OperationStatus` indicating success or failure.
///
/// # Safety
///
/// The caller must ensure that:
/// - `node` is a valid pointer to a `LogosBlockchainNode` instance
/// - The `LogosBlockchainNode` instance was created by this library
/// - The pointer will not be used after this function returns
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stop_node(node: *mut LogosBlockchainNode) -> OperationStatus {
    if node.is_null() {
        log::error!("Attempted to stop a null node pointer. This is a bug. Aborting.");
        return OperationStatus::NullPointer;
    }

    let node = unsafe { Box::from_raw(node) };
    node.stop()
}
