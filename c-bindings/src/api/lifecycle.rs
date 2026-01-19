use std::ffi::c_char;

use lb_node::{Config, get_services_to_start, run_node_from_config};
use tokio::runtime::Runtime;

use crate::{LogosBlockchainNode, api::PointerResult, errors::OperationStatus};

pub type InitializedLogosBlockchainNodeResult = PointerResult<LogosBlockchainNode, OperationStatus>;

/// Creates and starts a Logos blockchain node based on the provided
/// configuration file path.
///
/// # Arguments
///
/// - `config_path`: A pointer to a string representing the path to the
///   configuration file.
///
/// # Returns
///
/// An `InitializedLogosBlockchainNodeResult` containing either a pointer to the
/// initialized `LogosBlockchainNode` or an error code.
#[unsafe(no_mangle)]
pub extern "C" fn start_lb_node(
    config_path: *const c_char,
) -> InitializedLogosBlockchainNodeResult {
    initialize_lb_node(config_path).map_or_else(
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
///
/// # Returns
///
/// A `Result` containing either the initialized `LogosBlockchainNode` or an
/// error code.
fn initialize_lb_node(config_path: *const c_char) -> Result<LogosBlockchainNode, OperationStatus> {
    // TODO: Remove flags when dynamic run of services is implemented.
    let must_blend_service_group_start = true;
    let must_da_service_group_start = true;
    let config_path = unsafe { std::ffi::CStr::from_ptr(config_path) }
        .to_str()
        .map_err(|e| {
            eprintln!("Could not convert the config path to string: {e}");
            OperationStatus::InitializationError
        })?;
    let config_reader = std::fs::File::open(config_path).map_err(|e| {
        eprintln!("Could not open config file: {e}");
        OperationStatus::InitializationError
    })?;
    let config = serde_yaml::from_reader::<_, Config>(config_reader).map_err(|e| {
        eprintln!("Could not parse config file: {e}");
        OperationStatus::InitializationError
    })?;

    let rt = Runtime::new().unwrap();
    let app = run_node_from_config(config).map_err(|e| {
        eprintln!("Could not initialize Overwatch: {e}");
        OperationStatus::InitializationError
    })?;

    let app_handle = app.handle();

    rt.block_on(async {
        let services_to_start = get_services_to_start(
            &app,
            must_blend_service_group_start,
            must_da_service_group_start,
        )
        .await
        .map_err(|e| {
            eprintln!("Could not get services to start: {e}");
            OperationStatus::InitializationError
        })?;
        app_handle
            .start_service_sequence(services_to_start)
            .await
            .map_err(|e| {
                eprintln!("Could not start services: {e}");
                OperationStatus::InitializationError
            })?;
        Ok(())
    })?;

    Ok(LogosBlockchainNode::new(app, rt))
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
        eprintln!("Attempted to stop a null node pointer. This is a bug. Aborting.");
        return OperationStatus::NullPointer;
    }

    let node = unsafe { Box::from_raw(node) };
    node.stop()
}
