use std::ffi::c_char;

use nomos_node::{Config, get_services_to_start, run_node_from_config};
use tokio::runtime::Runtime;

use crate::{NomosNode, api::PointerResult, errors::NomosNodeErrorCode};

pub type InitializedNomosNodeResult = PointerResult<NomosNode, NomosNodeErrorCode>;

/// Creates and starts a Nomos node based on the provided configuration file
/// path.
///
/// # Arguments
///
/// - `config_path`: A pointer to a string representing the path to the
///   configuration file.
///
/// # Returns
///
/// An `InitializedNomosNodeResult` containing either a pointer to the
/// initialized `NomosNode` or an error code.
#[unsafe(no_mangle)]
pub extern "C" fn start_nomos_node(config_path: *const c_char) -> InitializedNomosNodeResult {
    initialize_nomos_node(config_path).map_or_else(
        InitializedNomosNodeResult::from_error,
        InitializedNomosNodeResult::from_value,
    )
}
/// Initializes and starts a Nomos node based on the provided configuration file
/// path.
///
/// # Arguments
///
/// - `config_path`: A pointer to a string representing the path to the
///   configuration file.
///
/// # Returns
///
/// A `Result` containing either the initialized `NomosNode` or an error code.
fn initialize_nomos_node(config_path: *const c_char) -> Result<NomosNode, NomosNodeErrorCode> {
    // TODO: Remove flags when dynamic run of services is implemented.
    let must_blend_service_group_start = true;
    let must_da_service_group_start = true;
    let config_path = unsafe { std::ffi::CStr::from_ptr(config_path) }
        .to_str()
        .map_err(|e| {
            eprintln!("Could not convert config path to string: {e}");
            NomosNodeErrorCode::CouldNotInitialize
        })?;
    let config_reader = std::fs::File::open(config_path).map_err(|e| {
        eprintln!("Could not open config file: {e}");
        NomosNodeErrorCode::CouldNotInitialize
    })?;
    let config = serde_yaml::from_reader::<_, Config>(config_reader).map_err(|e| {
        eprintln!("Could not parse config file: {e}");
        NomosNodeErrorCode::CouldNotInitialize
    })?;

    let rt = Runtime::new().unwrap();
    let app = run_node_from_config(config).map_err(|e| {
        eprintln!("Could not initialize overwatch: {e}");
        NomosNodeErrorCode::CouldNotInitialize
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
            NomosNodeErrorCode::CouldNotInitialize
        })?;
        app_handle
            .start_service_sequence(services_to_start)
            .await
            .map_err(|e| {
                eprintln!("Could not start services: {e}");
                NomosNodeErrorCode::CouldNotInitialize
            })?;
        Ok(())
    })?;

    Ok(NomosNode::new(app, rt))
}

/// Stops and frees the resources associated with the given Nomos node.
///
/// # Arguments
///
/// - `node`: A pointer to the `NomosNode` instance to be stopped.
///
/// # Returns
///
/// An `NomosNodeErrorCode` indicating success or failure.
///
/// # Safety
///
/// The caller must ensure that:
/// - `node` is a valid pointer to a `NomosNode` instance
/// - The `NomosNode` instance was created by this library
/// - The pointer will not be used after this function returns
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stop_node(node: *mut NomosNode) -> NomosNodeErrorCode {
    if node.is_null() {
        eprintln!("Attempted to stop a null node pointer. This is a bug. Aborting.");
        return NomosNodeErrorCode::NullPtr;
    }

    let node = unsafe { Box::from_raw(node) };
    node.stop()
}
