use std::{
    ffi::{CStr, c_char},
    net::Ipv4Addr,
    path::PathBuf,
    slice,
    str::FromStr as _,
};

use lb_node::cli::{EmbeddedInitArgs, InitArgs, MigrateArgs, ParticipateArgs, UpdateArgs};
use multiaddr::Multiaddr;
use tokio::runtime::Runtime;

use crate::{OperationStatus, logging, return_error_if_null_pointer};

/// Converts a non-null C string pointer into a [`PathBuf`].
///
/// # Safety
///
/// The pointer must be non-null and point to a valid NUL-terminated C string.
pub(crate) unsafe fn cstr_to_path(pointer: *const c_char) -> PathBuf {
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .to_string()
        .into()
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
    pub state_path: *const c_char,
    pub storage_path: *const c_char,
    pub logs_path: *const c_char,
    pub ibd: *const bool,
    pub log_filter: *const c_char,
    pub kms_file: *const c_char,
}

impl From<GenerateConfigArgs> for EmbeddedInitArgs {
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

        // ---- state_path ----
        if !value.state_path.is_null() {
            let state_path = unsafe { CStr::from_ptr(value.state_path) };
            init_args.state_path = Some(state_path.to_string_lossy().to_string().into());
        }

        // ---- storage_path ----
        if !value.storage_path.is_null() {
            let storage_path = unsafe { CStr::from_ptr(value.storage_path) };
            init_args.storage_path = Some(storage_path.to_string_lossy().to_string().into());
        }

        // ---- logs_path ----
        if !value.logs_path.is_null() {
            let logs_path = unsafe { CStr::from_ptr(value.logs_path) };
            init_args.logs_path = Some(logs_path.to_string_lossy().to_string().into());
        }

        // ---- ibd ----
        if !value.ibd.is_null() {
            init_args.ibd = unsafe { *value.ibd };
        }

        // ---- log_filter ----
        if !value.log_filter.is_null() {
            let log_filter = unsafe { CStr::from_ptr(value.log_filter) };
            init_args.log_filter = Some(log_filter.to_string_lossy().to_string());
        }

        // ---- kms_file ----
        if !value.kms_file.is_null() {
            let kms_file = unsafe { CStr::from_ptr(value.kms_file) };
            init_args.kms_file = Some(kms_file.to_string_lossy().to_string().into());
        }

        init_args
    }
}

#[must_use]
pub fn generate_config_sync(args: EmbeddedInitArgs) -> OperationStatus {
    let init_args: InitArgs = args.into();
    let runtime = Runtime::new().expect("Failed to create Tokio runtime.");
    let run_result = runtime.block_on(async move { lb_node::cli::config::init::run(init_args) });
    match run_result {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            logging::error!("generate_config_sync", "Error generating config: {error:?}");
            OperationStatus::ConfigurationError
        }
    }
}

/// Generates the user config file.
///
/// # Arguments
///
/// - `args`: A [`GenerateConfigArgs`] struct containing the arguments to be
///   used for generating the config file.
///
/// # Returns
///
/// An [`OperationStatus`] indicating the result of the operation.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn generate_user_config(args: GenerateConfigArgs) -> OperationStatus {
    let init_args = EmbeddedInitArgs::from(args);
    generate_config_sync(init_args)
}

/// Updates an existing user config file with keys from a keystore file,
/// equivalent to the `update-config` CLI command. Runs non-interactively
/// (existing files are overwritten without confirmation).
///
/// # Arguments
///
/// - `user_config_path`: Path to the user config YAML file.
/// - `keystore_path`: Path to the keystore YAML file.
///
/// # Returns
///
/// An [`OperationStatus`] indicating the result of the operation.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid NUL-terminated C strings.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn update_user_config(
    user_config_path: *const c_char,
    keystore_path: *const c_char,
) -> OperationStatus {
    return_error_if_null_pointer!("update_user_config", user_config_path);
    return_error_if_null_pointer!("update_user_config", keystore_path);

    let args = UpdateArgs::new(
        unsafe { cstr_to_path(user_config_path) },
        unsafe { cstr_to_path(keystore_path) },
        true,
    );

    match lb_node::cli::config::update::run(args) {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            logging::error!("update_user_config", "Error updating config: {error:?}");
            OperationStatus::ConfigurationError
        }
    }
}

/// Generates a new user config file from an existing keystore file,
/// equivalent to the `migrate-config` CLI command.
///
/// # Arguments
///
/// - `output_path`: Output path for the generated user config YAML file. Must
///   not exist yet.
/// - `keystore_path`: Path to the existing keystore YAML file.
///
/// # Returns
///
/// An [`OperationStatus`] indicating the result of the operation.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid NUL-terminated C strings.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migrate_user_config(
    output_path: *const c_char,
    keystore_path: *const c_char,
) -> OperationStatus {
    return_error_if_null_pointer!("migrate_user_config", output_path);
    return_error_if_null_pointer!("migrate_user_config", keystore_path);

    let args = MigrateArgs::new(unsafe { cstr_to_path(output_path) }, unsafe {
        cstr_to_path(keystore_path)
    });

    match lb_node::cli::config::migrate::run(args) {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            logging::error!("migrate_user_config", "Error migrating config: {error:?}");
            OperationStatus::ConfigurationError
        }
    }
}

/// Migrates a 0.1.2 config file to a new user config and keystore, equivalent
/// to the `migrate-from-0.1.2` CLI command.
///
/// # Arguments
///
/// - `new_config_path`: Output path for the generated user config YAML file.
///   Must not exist yet.
/// - `old_config_path`: Path to the existing 0.1.2 config YAML file.
/// - `keystore_path`: Output path for the generated keystore YAML file. Must
///   not exist yet.
///
/// # Returns
///
/// An [`OperationStatus`] indicating the result of the operation.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid NUL-terminated C strings.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migrate_user_config_0_1_2(
    new_config_path: *const c_char,
    old_config_path: *const c_char,
    keystore_path: *const c_char,
) -> OperationStatus {
    return_error_if_null_pointer!("migrate_user_config_0_1_2", new_config_path);
    return_error_if_null_pointer!("migrate_user_config_0_1_2", old_config_path);
    return_error_if_null_pointer!("migrate_user_config_0_1_2", keystore_path);

    let args = lb_node::cli::config::migrate_0_1_2::MigrateArgs::new(
        unsafe { cstr_to_path(new_config_path) },
        unsafe { cstr_to_path(old_config_path) },
        unsafe { cstr_to_path(keystore_path) },
    );

    match lb_node::cli::config::migrate_0_1_2::run(args) {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            logging::error!(
                "migrate_user_config_0_1_2",
                "Error migrating config: {error:?}"
            );
            OperationStatus::ConfigurationError
        }
    }
}

/// Generates `participation_data.yaml` from a user config and keystore,
/// equivalent to the `participate` CLI command.
///
/// # Arguments
///
/// - `config_path`: Path to the user config YAML file.
/// - `keystore_path`: Path to the keystore YAML file.
/// - `output_dir`: Output directory for `participation_data.yaml`.
/// - `external_address`: Optional (nullable) public IPv4 address of the node,
///   required when the blend listening address is unspecified (0.0.0.0).
///
/// # Returns
///
/// An [`OperationStatus`] indicating the result of the operation.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all non-null pointers are valid NUL-terminated C strings.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn participate(
    config_path: *const c_char,
    keystore_path: *const c_char,
    output_dir: *const c_char,
    external_address: *const c_char,
) -> OperationStatus {
    return_error_if_null_pointer!("participate", config_path);
    return_error_if_null_pointer!("participate", keystore_path);
    return_error_if_null_pointer!("participate", output_dir);

    let external_address = if external_address.is_null() {
        None
    } else {
        let address = unsafe { CStr::from_ptr(external_address) }.to_string_lossy();
        match address.parse::<Ipv4Addr>() {
            Ok(address) => Some(address),
            Err(error) => {
                logging::error!(
                    "participate",
                    "Invalid external address '{address}': {error}"
                );
                return OperationStatus::ValidationError;
            }
        }
    };

    let args = ParticipateArgs {
        config: unsafe { cstr_to_path(config_path) },
        keystore: unsafe { cstr_to_path(keystore_path) },
        output: unsafe { cstr_to_path(output_dir) },
        external_address,
    };

    match lb_node::cli::participate::run(&args) {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            logging::error!(
                "participate",
                "Error generating participation data: {error:?}"
            );
            OperationStatus::ConfigurationError
        }
    }
}

#[cfg(test)]
mod test {
    use std::{ffi::CString, path::Path};

    use tempfile::TempDir;

    use super::*;
    use crate::api::{
        free_cstring,
        keys::{KeyType, add_key, generate_key, remove_key},
        peer::get_peer_id,
    };

    fn cstring(path: &Path) -> CString {
        CString::new(path.to_string_lossy().as_bytes()).expect("Path should not contain NUL")
    }

    #[test]
    fn test_config_and_key_commands_roundtrip() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("user_config.yaml");
        let keystore_path = temp_dir.path().join("keystore.yaml");

        // init-config
        let status = generate_config_sync(EmbeddedInitArgs {
            output: config_path.clone(),
            kms_file: Some(keystore_path.clone()),
            ..Default::default()
        });
        assert!(status.is_ok(), "Failed to generate config: {status:?}");
        assert!(config_path.exists());
        assert!(keystore_path.exists());

        let config_c = cstring(&config_path);
        let keystore_c = cstring(&keystore_path);

        // update-config
        let status = unsafe { update_user_config(config_c.as_ptr(), keystore_c.as_ptr()) };
        assert!(status.is_ok(), "Failed to update config: {status:?}");

        // generate-key
        let generated_title = CString::new("GeneratedKey").expect("Valid CString");
        let result = unsafe {
            generate_key(
                config_c.as_ptr(),
                keystore_c.as_ptr(),
                KeyType::Zk,
                generated_title.as_ptr(),
            )
        };
        assert!(result.is_ok(), "Failed to generate key: {:?}", result.error);
        let key_id = unsafe { CStr::from_ptr(result.value) }
            .to_str()
            .expect("Key id should be valid UTF-8")
            .to_owned();
        assert!(!key_id.is_empty());
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        // add-key
        let key_hex = CString::new("11".repeat(32)).expect("Valid CString");
        let added_title = CString::new("AddedKey").expect("Valid CString");
        let status = unsafe {
            add_key(
                config_c.as_ptr(),
                keystore_c.as_ptr(),
                KeyType::Ed25519,
                key_hex.as_ptr(),
                added_title.as_ptr(),
            )
        };
        assert!(status.is_ok(), "Failed to add key: {status:?}");

        // remove-key
        let status =
            unsafe { remove_key(config_c.as_ptr(), keystore_c.as_ptr(), added_title.as_ptr()) };
        assert!(status.is_ok(), "Failed to remove key: {status:?}");

        // get-peer-id
        let result = unsafe { get_peer_id(config_c.as_ptr()) };
        assert!(result.is_ok(), "Failed to get peer id: {:?}", result.error);
        let peer_id = unsafe { CStr::from_ptr(result.value) }
            .to_str()
            .expect("Peer id should be valid UTF-8")
            .to_owned();
        assert!(!peer_id.is_empty());
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        // participate
        let output_dir = temp_dir.path().join("participation");
        let output_c = cstring(&output_dir);
        let external_address = CString::new("203.0.113.7").expect("Valid CString");
        let status = unsafe {
            participate(
                config_c.as_ptr(),
                keystore_c.as_ptr(),
                output_c.as_ptr(),
                external_address.as_ptr(),
            )
        };
        assert!(
            status.is_ok(),
            "Failed to generate participation data: {status:?}"
        );
        assert!(output_dir.join("participation_data.yaml").exists());

        // migrate-config
        let migrated_path = temp_dir.path().join("migrated_config.yaml");
        let migrated_c = cstring(&migrated_path);
        let status = unsafe { migrate_user_config(migrated_c.as_ptr(), keystore_c.as_ptr()) };
        assert!(status.is_ok(), "Failed to migrate config: {status:?}");
        assert!(migrated_path.exists());
    }
}
