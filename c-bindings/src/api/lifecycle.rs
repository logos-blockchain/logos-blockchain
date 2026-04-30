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

use crate::{
    LogosBlockchainNode,
    errors::OperationStatus,
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
/// - `deployment`: A pointer to a string representing either a well-known
///   deployment name (e.g., "devnet") or a path to a deployment YAML file. If
///   null, defaults to "devnet".
///
/// # Returns
///
/// An [`FfiInitializedLogosBlockchainNodeResult`] containing either a pointer
/// to the initialized [`LogosBlockchainNode`] or an error code.
#[unsafe(no_mangle)]
pub extern "C" fn start_lb_node(
    config_path: *const c_char,
    deployment: *const c_char,
) -> FfiInitializedLogosBlockchainNodeResult {
    initialize_lb_node(config_path, deployment).map_or_else(
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
/// - `deployment`: A pointer to a string representing either a well-known
///   deployment name (e.g., "devnet") or a path to a deployment YAML file. If
///   null, defaults to "devnet".
///
/// # Returns
///
/// A [`Result`] containing either the initialized [`LogosBlockchainNode`] or an
/// error code.
fn initialize_lb_node(
    config_path: *const c_char,
    deployment: *const c_char,
) -> StatusResult<LogosBlockchainNode> {
    let run_config = RunConfig {
        deployment: get_deployment_config(deployment)?,
        user: get_user_config(config_path)?,
    };

    let runtime = Runtime::new().expect("Failed to create Tokio runtime");
    let app = run_node_from_config(run_config, Some(runtime.handle().clone())).map_err(|e| {
        log::error!("Could not initialize Overwatch: {e}");
        OperationStatus::InitializationError
    })?;

    let app_handle = app.handle();

    runtime.block_on(async {
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

    Ok(LogosBlockchainNode::new(app, runtime))
}

fn get_user_config(config_path: *const c_char) -> StatusResult<UserConfig> {
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

fn get_deployment_config(deployment_arg: *const c_char) -> StatusResult<DeploymentSettings> {
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
    use std::{path::PathBuf, sync::LazyLock};

    use crate::api::lifecycle::{start_lb_node, stop_node};

    static REPOSITORY_ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
        let crate_dir = env!("CARGO_MANIFEST_DIR");
        let crate_path = PathBuf::from(crate_dir);
        crate_path
            .parent()
            .expect("Failed to get the parent directory of crate.")
            .to_path_buf()
    });
    static CRATE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
        let dir = REPOSITORY_ROOT.join("c-bindings");
        assert!(dir.exists());
        dir
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

    struct NodeStateGuard {
        location: PathBuf,
        existed_before: bool,
    }

    impl NodeStateGuard {
        #[must_use]
        pub fn new(location: PathBuf) -> Self {
            let exists = location.exists();
            Self {
                location,
                existed_before: exists,
            }
        }

        #[must_use]
        pub fn from_current_crate() -> Self {
            let current_dir = CRATE_DIR.clone();
            let state_dir = current_dir.join("state");
            Self::new(state_dir)
        }

        fn cleanup(&self) {
            if !self.existed_before {
                drop(std::fs::remove_dir_all(&self.location));
            }
        }
    }

    impl Drop for NodeStateGuard {
        fn drop(&mut self) {
            self.cleanup();
        }
    }

    #[test]
    fn test_basic_lifecycle() {
        let _guard = NodeStateGuard::from_current_crate();

        let start_status = start_lb_node(
            STANDALONE_NODE_CONFIG_PATH
                .to_str()
                .unwrap()
                .as_ptr()
                .cast::<i8>(),
            STANDALONE_DEPLOYMENT_CONFIG_PATH
                .to_str()
                .unwrap()
                .as_ptr()
                .cast::<i8>(),
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
