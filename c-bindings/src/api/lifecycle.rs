use std::ffi::c_char;

use lb_node::{
    UserConfig,
    config::{RunConfig, deployment::DeploymentSettings},
    get_services_to_start, run_node_from_config,
};
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
use tokio::runtime::Runtime;

use crate::{
    LogosBlockchainNode,
    errors::OperationStatus,
    logging,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer,
};

pub type FfiInitializedLogosBlockchainNodeResult = FfiStatusResult<*mut LogosBlockchainNode>;

/// Creates and starts a Logos blockchain node based on the provided
/// configuration file path.
///
/// # Arguments
///
/// - `config_path`: A pointer to a string representing the path to the
///   configuration file.
/// - `custom_deployment_path`: An optional pointer to a string representing the
///   path to the custom deployment configuration file. If null, defaults to
///   binary default deployment (e.g., devnet for release candidates and testnet
///   for releases).
///
/// # Returns
///
/// An [`FfiInitializedLogosBlockchainNodeResult`] containing either a pointer
/// to the initialized [`LogosBlockchainNode`] or an error code.
#[unsafe(no_mangle)]
pub extern "C" fn start_lb_node(
    config_path: *const c_char,
    custom_deployment_path: *const c_char,
) -> FfiInitializedLogosBlockchainNodeResult {
    initialize_lb_node(config_path, custom_deployment_path).map_or_else(
        FfiInitializedLogosBlockchainNodeResult::err,
        FfiInitializedLogosBlockchainNodeResult::from_value,
    )
}

/// Initializes and starts a Logos blockchain node based on the provided
/// configuration file path.
///
/// # Arguments
///
/// - `config_path`: A pointer to a string representing the path to the
///   configuration file.
/// - `custom_deployment_path`: An optional pointer to a string representing the
///   path to the custom deployment configuration file. If null, defaults to
///   binary default deployment (e.g., devnet for release candidates and testnet
///   for releases).
///
/// # Returns
///
/// A [`Result`] containing either the initialized [`LogosBlockchainNode`] or an
/// error code.
fn initialize_lb_node(
    config_path: *const c_char,
    custom_deployment_path: *const c_char,
) -> StatusResult<LogosBlockchainNode> {
    let run_config = RunConfig {
        deployment: get_deployment_config(custom_deployment_path)?,
        user: get_user_config(config_path)?,
    };

    let runtime = Runtime::new().expect("Failed to create Tokio runtime");
    let app = run_node_from_config(run_config, Some(runtime.handle().clone())).map_err(|e| {
        logging::error!("initialize_lb_node", "Could not initialize Overwatch: {e}");
        OperationStatus::InitializationError
    })?;

    let app_handle = app.handle();

    runtime.block_on(async {
        let services_to_start = get_services_to_start(&app).await.map_err(|e| {
            logging::error!("initialize_lb_node", "Could not get services to start: {e}");
            OperationStatus::InitializationError
        })?;
        app_handle
            .start_service_sequence(services_to_start)
            .await
            .map_err(|e| {
                logging::error!("initialize_lb_node", "Could not start services: {e}");
                OperationStatus::InitializationError
            })?;
        Ok(())
    })?;

    Ok(LogosBlockchainNode::new(app, runtime))
}

fn get_user_config(config_path: *const c_char) -> StatusResult<UserConfig> {
    let user_config_path = unsafe { std::ffi::CStr::from_ptr(config_path) }
        .to_str()
        .map_err(|e| {
            logging::error!(
                "get_user_config",
                "Could not convert the config path to string: {e}"
            );
            OperationStatus::InitializationError
        })?;
    deserialize_value_at_path::<UserConfig>(user_config_path.as_ref(), OnUnknownKeys::Fail).map_err(
        |e| {
            logging::error!("get_user_config", "Could not parse config file: {e}");
            OperationStatus::InitializationError
        },
    )
}

fn get_deployment_config(
    custom_deployment_path: *const c_char,
) -> StatusResult<DeploymentSettings> {
    if custom_deployment_path.is_null() {
        Ok(DeploymentSettings::default())
    } else {
        let custom_deployment_path = unsafe { std::ffi::CStr::from_ptr(custom_deployment_path) }
            .to_str()
            .map_err(|e| {
                logging::error!(
                    "get_deployment_config",
                    "Could not convert custom deployment path to string: {e}"
                );
                OperationStatus::InitializationError
            })?;
        Ok(deserialize_value_at_path::<DeploymentSettings>(
            custom_deployment_path.as_ref(),
            OnUnknownKeys::Fail,
        )
        .map_err(|e| {
            logging::error!(
                "get_deployment_config",
                "Could not parse deployment file: {e}"
            );
            OperationStatus::InitializationError
        })?)
    }
}

/// Stops and frees the resources associated with the given Logos blockchain
/// node.
///
/// # Arguments
///
/// - `node`: A pointer to the [`LogosBlockchainNode`] instance to be stopped.
///
/// # Returns
///
/// An [`OperationStatus`] indicating success or failure.
///
/// # Safety
///
/// The caller must ensure that:
/// - `node` is a valid pointer to a [`LogosBlockchainNode`] instance
/// - The [`LogosBlockchainNode`] instance was created by this library
/// - The pointer will not be used after this function returns
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stop_node(node: *mut LogosBlockchainNode) -> OperationStatus {
    return_error_if_null_pointer!("stop_node", node);
    let node = unsafe { Box::from_raw(node) };
    node.stop()
}

#[cfg(test)]
mod test {
    use std::{ffi::CString, path::PathBuf, sync::LazyLock};

    use lb_node::UserConfig;
    use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
    use tempfile::TempDir;

    use crate::api::lifecycle::{start_lb_node, stop_node};

    static REPOSITORY_ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
        let crate_dir = env!("CARGO_MANIFEST_DIR");
        let crate_path = PathBuf::from(crate_dir);
        crate_path
            .parent()
            .expect("Failed to get the parent directory of crate.")
            .to_path_buf()
    });
    static NODE_DIR: LazyLock<PathBuf> = LazyLock::new(|| REPOSITORY_ROOT.join("nodes/node"));
    static STANDALONE_NODE_CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
        let file = NODE_DIR.join("standalone-node-config.yaml");
        assert!(file.exists());
        file
    });
    static STANDALONE_DEPLOYMENT_CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
        let file = NODE_DIR.join("standalone-deployment-config.yaml");
        assert!(file.exists());
        file
    });

    struct TestConfigPaths {
        _temp_dir: TempDir,
        node_config: CString,
        deployment_config: CString,
    }

    impl TestConfigPaths {
        #[must_use]
        fn new() -> Self {
            let temp_dir = TempDir::new().expect("Failed to create temp dir for lifecycle test");
            let log_dir = temp_dir.path().join("state/logs");
            std::fs::create_dir_all(&log_dir).expect("Failed to create isolated log dir");

            let node_config_path = temp_dir.path().join("standalone-node-config.yaml");
            let deployment_config_path = temp_dir.path().join("standalone-deployment-config.yaml");

            let state_dir_path = temp_dir.path().join("state");
            let mut node_config = deserialize_value_at_path::<UserConfig>(
                STANDALONE_NODE_CONFIG_PATH.as_path(),
                OnUnknownKeys::Fail,
            )
            .expect("Standalone user config should deserialize");
            node_config.state.base_folder = state_dir_path;
            node_config.api.backend.listen_address = "127.0.0.1:0"
                .parse()
                .expect("Local address should be correct");

            let node_config_yaml = serde_yaml::to_string(&node_config)
                .expect("Standalone node config should be written to file");
            std::fs::write(&node_config_path, node_config_yaml)
                .expect("Failed to write isolated node config");
            std::fs::copy(
                STANDALONE_DEPLOYMENT_CONFIG_PATH.as_path(),
                &deployment_config_path,
            )
            .expect("Failed to copy standalone deployment config");

            let node_config = CString::new(node_config_path.to_string_lossy().as_bytes())
                .expect("Node config path should not contain NUL");
            let deployment_config =
                CString::new(deployment_config_path.to_string_lossy().as_bytes())
                    .expect("Deployment config path should not contain NUL");

            Self {
                _temp_dir: temp_dir,
                node_config,
                deployment_config,
            }
        }
    }

    #[test]
    fn test_basic_lifecycle() {
        let test_paths = TestConfigPaths::new();

        let start_status = start_lb_node(
            test_paths.node_config.as_ptr(),
            test_paths.deployment_config.as_ptr(),
        );

        assert!(
            start_status.is_ok(),
            "Failed to start node: {:?}",
            start_status.error
        );
        let node = start_status.value;

        let stop_status = unsafe { stop_node(node) };

        assert!(stop_status.is_ok(), "Failed to stop node: {stop_status:?}");
    }
}
