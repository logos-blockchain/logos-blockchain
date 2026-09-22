use std::{
    ffi::{CStr, CString, c_char},
    net::Ipv4Addr,
    path::PathBuf,
    ptr, slice,
    str::FromStr as _,
};

use lb_node::cli::{
    EmbeddedInitArgs, InitArgs, MigrateArgs, ParticipateArgs, UpdateArgs, config::merge::MergeFlags,
};
use multiaddr::Multiaddr;
use tokio::runtime::Runtime;

use crate::{
    OperationStatus, errors::OperationStatusCode, result::FfiStatusResult,
    return_error_if_null_pointer,
};

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
    pub skip_ibd: *const bool,
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

        // ---- skip_ibd ----
        if !value.skip_ibd.is_null() {
            init_args.skip_ibd = unsafe { *value.skip_ibd };
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
        Ok(()) => OperationStatus::OK,
        Err(error) => OperationStatus::error(
            OperationStatusCode::ConfigurationError,
            format!("Error generating config: {error:?}"),
        ),
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
    return_error_if_null_pointer!(user_config_path);
    return_error_if_null_pointer!(keystore_path);

    let args = UpdateArgs::new(
        unsafe { cstr_to_path(user_config_path) },
        unsafe { cstr_to_path(keystore_path) },
        true,
    );

    match lb_node::cli::config::update::run(args) {
        Ok(()) => OperationStatus::OK,
        Err(error) => OperationStatus::error(
            OperationStatusCode::ConfigurationError,
            format!("Error updating config: {error:?}"),
        ),
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
    return_error_if_null_pointer!(output_path);
    return_error_if_null_pointer!(keystore_path);

    let args = MigrateArgs::new(unsafe { cstr_to_path(output_path) }, unsafe {
        cstr_to_path(keystore_path)
    });

    match lb_node::cli::config::migrate::run(args) {
        Ok(()) => OperationStatus::OK,
        Err(error) => OperationStatus::error(
            OperationStatusCode::ConfigurationError,
            format!("Error migrating config: {error:?}"),
        ),
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
    return_error_if_null_pointer!(new_config_path);
    return_error_if_null_pointer!(old_config_path);
    return_error_if_null_pointer!(keystore_path);

    let args = lb_node::cli::config::migrate_0_1_2::MigrateArgs::new(
        unsafe { cstr_to_path(new_config_path) },
        unsafe { cstr_to_path(old_config_path) },
        unsafe { cstr_to_path(keystore_path) },
    );

    match lb_node::cli::config::migrate_0_1_2::run(args) {
        Ok(()) => OperationStatus::OK,
        Err(error) => OperationStatus::error(
            OperationStatusCode::ConfigurationError,
            format!("Error migrating config: {error:?}"),
        ),
    }
}

/// Merge behaviour flags. Mirror of [`MergeFlags`] for the C API.
#[repr(C)]
pub struct MergeConfigFlags {
    /// Insert source keys missing from the destination instead of reporting
    /// them.
    pub source_insert_missing: bool,
    /// Insert extra keys missing from the destination instead of reporting
    /// them.
    pub extra_insert_missing: bool,
}

impl From<MergeConfigFlags> for MergeFlags {
    fn from(value: MergeConfigFlags) -> Self {
        Self {
            source_insert_missing: value.source_insert_missing,
            extra_insert_missing: value.extra_insert_missing,
        }
    }
}

/// Result type for [`merge_user_config`].
///
/// On success, `value` is either null (no merge conflicts) or a pointer to a
/// NUL-terminated C string with one merge conflict per line.
pub type FfiMergeUserConfigResult = FfiStatusResult<*mut c_char>;

/// Merges the values of a source config file, and optionally extra YAML values,
/// onto a destination config file.
///
/// Extra values take precedence over source values.
///
/// Merges `source`, then `extra`, onto `destination`.
///
/// - Maps are merged key by key.
/// - Lists and tagged values are replaced whole: changes nested inside them are
///   neither merged nor reported as conflicts.
///
/// The destination file is overwritten with the result.
///
/// # Requirements
///
/// Running [`migrate_user_config`] before merging (calling this function) is
/// recommended, it will cleanly handle the keystore migration.
///
/// # Arguments
///
/// - `source_path`: Path to the config YAML file whose values are merged.
/// - `destination_path`: Path to the config YAML file merged onto and
///   overwritten.
/// - `extra_yaml`: Optional (nullable) YAML string with extra values.
/// - `flags`: A [`MergeConfigFlags`] struct with the merge behavior flags.
///
/// # Returns
///
/// A [`FfiMergeUserConfigResult`] containing the merge conflicts report (null
/// if there are none) on success, or an [`OperationStatus`] error if the merge
/// could not run.
///
/// Conflicts do not mean the merge failed: the destination file is still
/// written. Each conflict is a value that could not be merged, and the
/// destination keeps its own value for that key.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all non-null pointers are valid NUL-terminated C strings.
///
/// # Memory Management
///
/// When non-null, the returned report is allocated by this function. The
/// caller must free it using the [`free_cstring`](super::free_cstring)
/// function.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn merge_user_config(
    source_path: *const c_char,
    destination_path: *const c_char,
    extra_yaml: *const c_char,
    flags: MergeConfigFlags,
) -> FfiMergeUserConfigResult {
    return_error_if_null_pointer!(source_path);
    return_error_if_null_pointer!(destination_path);

    let extra_yaml = if extra_yaml.is_null() {
        None
    } else {
        let extra_yaml = unsafe { CStr::from_ptr(extra_yaml) }.to_string_lossy();
        match serde_yaml::from_str(&extra_yaml) {
            Ok(extra) => Some(extra),
            Err(error) => {
                return FfiMergeUserConfigResult::err(OperationStatus::error(
                    OperationStatusCode::ValidationError,
                    format!("Invalid extra YAML: {error}"),
                ));
            }
        }
    };

    let flags = MergeFlags::from(flags);

    let conflicts = match lb_node::cli::config::merge::run(
        &unsafe { cstr_to_path(source_path) },
        &unsafe { cstr_to_path(destination_path) },
        extra_yaml,
        &flags,
    ) {
        Ok(conflicts) => conflicts,
        Err(error) => {
            return FfiMergeUserConfigResult::err(OperationStatus::error(
                OperationStatusCode::ConfigurationError,
                format!("Error merging config: {error:?}"),
            ));
        }
    };

    if conflicts.is_empty() {
        return FfiMergeUserConfigResult::ok(ptr::null_mut());
    }

    let report = conflicts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    match CString::new(report) {
        Ok(report) => FfiMergeUserConfigResult::ok(report.into_raw()),
        Err(error) => FfiMergeUserConfigResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create conflicts report: {error}"),
        )),
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
    return_error_if_null_pointer!(config_path);
    return_error_if_null_pointer!(keystore_path);
    return_error_if_null_pointer!(output_dir);

    let external_address = if external_address.is_null() {
        None
    } else {
        let address = unsafe { CStr::from_ptr(external_address) }.to_string_lossy();
        match address.parse::<Ipv4Addr>() {
            Ok(address) => Some(address),
            Err(error) => {
                return OperationStatus::error(
                    OperationStatusCode::ValidationError,
                    format!("Invalid external address '{address}': {error}"),
                );
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
        Ok(()) => OperationStatus::OK,
        Err(error) => OperationStatus::error(
            OperationStatusCode::ConfigurationError,
            format!("Error generating participation data: {error:?}"),
        ),
    }
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::api::{
        free_cstring,
        keys::{KeyType, add_key, generate_key, remove_key},
        peer::get_peer_id,
    };

    const NO_INSERT: MergeConfigFlags = MergeConfigFlags {
        source_insert_missing: false,
        extra_insert_missing: false,
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

    #[test]
    fn test_merge_user_config_writes_destination_and_returns_report() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let source_path = temp_dir.path().join("source.yaml");
        let destination_path = temp_dir.path().join("destination.yaml");
        std::fs::write(&source_path, "{ a: 2, b: 2 }").expect("Failed to write source");
        std::fs::write(&destination_path, "{ a: 1, c: 1 }").expect("Failed to write destination");

        let source_c = cstring(&source_path);
        let destination_c = cstring(&destination_path);
        let extra_c = CString::new("c: 3").expect("Valid CString");

        let result = unsafe {
            merge_user_config(
                source_c.as_ptr(),
                destination_c.as_ptr(),
                extra_c.as_ptr(),
                NO_INSERT,
            )
        };
        assert!(result.is_ok(), "Failed to merge config: {:?}", result.error);
        let report = unsafe { CStr::from_ptr(result.value) }
            .to_str()
            .expect("Report should be valid UTF-8")
            .to_owned();
        assert_eq!(
            report,
            "Key 'b' not found in new config. Value in old config: 2"
        );
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        let destination: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(&destination_path).expect("Failed to read destination"),
        )
        .expect("Destination should be valid YAML");
        let expected: serde_yaml::Value =
            serde_yaml::from_str("{ a: 2, c: 3 }").expect("Valid YAML");
        assert_eq!(destination, expected);
    }

    #[test]
    fn test_merge_user_config_returns_null_report_without_conflicts() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let source_path = temp_dir.path().join("source.yaml");
        let destination_path = temp_dir.path().join("destination.yaml");
        std::fs::write(&source_path, "a: 2").expect("Failed to write source");
        std::fs::write(&destination_path, "a: 1").expect("Failed to write destination");

        let source_c = cstring(&source_path);
        let destination_c = cstring(&destination_path);

        let result = unsafe {
            merge_user_config(
                source_c.as_ptr(),
                destination_c.as_ptr(),
                ptr::null(),
                NO_INSERT,
            )
        };
        assert!(result.is_ok(), "Failed to merge config: {:?}", result.error);
        assert!(result.value.is_null());
    }

    #[test]
    fn test_merge_user_config_rejects_invalid_extra_yaml() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let source_path = temp_dir.path().join("source.yaml");
        let destination_path = temp_dir.path().join("destination.yaml");
        std::fs::write(&source_path, "a: 2").expect("Failed to write source");
        std::fs::write(&destination_path, "a: 1").expect("Failed to write destination");

        let source_c = cstring(&source_path);
        let destination_c = cstring(&destination_path);
        let extra_c = CString::new("a: [").expect("Valid CString");

        let result = unsafe {
            merge_user_config(
                source_c.as_ptr(),
                destination_c.as_ptr(),
                extra_c.as_ptr(),
                NO_INSERT,
            )
        };
        assert_eq!(result.error.code, OperationStatusCode::ValidationError);
        let destination =
            std::fs::read_to_string(&destination_path).expect("Failed to read destination");
        assert_eq!(destination, "a: 1");
    }
}
