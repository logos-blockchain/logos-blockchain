use std::{
    ffi::{CStr, c_char},
    net::Ipv4Addr,
    num::NonZeroU16,
    path::PathBuf,
    ptr, slice,
    str::FromStr as _,
};

use lb_c_macros::panic_to_error;
use lb_node::cli::{EmbeddedInitArgs, InitArgs, ParticipateArgs};
use multiaddr::Multiaddr;
use tokio::runtime::Runtime;

use crate::{
    OperationStatus,
    api::{deployment::to_c_string, lifecycle::resolve_run_config},
    errors::OperationStatusCode,
    result::FfiStatusResult,
    return_error_if_null_pointer, unwrap_or_return_error,
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

impl GenerateConfigArgs {
    /// Reads the arguments into the node's own init arguments. Null pointers
    /// leave the corresponding default in place.
    ///
    /// # Safety
    ///
    /// Every non-null pointer must be valid for its type: the strings
    /// NUL-terminated, and `initial_peers` pointing to `initial_peers_count`
    /// entries.
    unsafe fn into_init_args(self) -> EmbeddedInitArgs {
        let value = self;
        let mut init_args = EmbeddedInitArgs::default();

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
            // Read as a byte: C can store any value in a `bool`, and anything
            // but 0 or 1 is not a valid Rust `bool`.
            init_args.skip_ibd = unsafe { *value.skip_ibd.cast::<u8>() } != 0;
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
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn generate_user_config(args: GenerateConfigArgs) -> OperationStatus {
    let init_args = unsafe { args.into_init_args() };
    generate_config_sync(init_args)
}

/// Generates `participation_data.yaml` from a user config and its keystore,
/// equivalent to the `participate` CLI command.
///
/// # Arguments
///
/// - `config_path`: Path to the user config YAML file.
/// - `output_dir`: Output directory for `participation_data.yaml`.
/// - `external_address`: Optional (nullable) public IPv4 address of the node,
///   required when the blend listening address is unspecified (0.0.0.0).
/// - `external_port`: Optional (nullable) public blend port of the node,
///   required when the blend listening address uses port 0. The port must not
///   be 0.
///
/// # Returns
///
/// An [`OperationStatus`] indicating the result of the operation.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all non-null string pointers are valid NUL-terminated C
/// strings, and that `external_port`, if non-null, points to a valid `u16`.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn participate(
    config_path: *const c_char,
    output_dir: *const c_char,
    external_address: *const c_char,
    external_port: *const u16,
) -> OperationStatus {
    return_error_if_null_pointer!(config_path);
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

    let external_port = if external_port.is_null() {
        None
    } else {
        let Some(port) = NonZeroU16::new(unsafe { *external_port }) else {
            return OperationStatus::error(
                OperationStatusCode::ValidationError,
                "Invalid external port '0': the port must not be 0",
            );
        };
        Some(port)
    };

    let args = ParticipateArgs {
        config: unsafe { cstr_to_path(config_path) },
        output: unsafe { cstr_to_path(output_dir) },
        external_address,
        external_port,
    };

    match lb_node::cli::participate::run(&args) {
        Ok(()) => OperationStatus::OK,
        Err(error) => OperationStatus::error(
            OperationStatusCode::ConfigurationError,
            format!("Error generating participation data: {error:?}"),
        ),
    }
}

pub type FfiShowConfigResult = FfiStatusResult<*mut c_char>;

/// The config a node started from `config_path` would run with, as YAML with
/// the secrets redacted: the keystore merged in and the environment overrides
/// applied, like `--show-config` on the node binary. Starts nothing.
///
/// # Safety
///
/// `config_path` must be a valid NUL-terminated C string.
///
/// # Memory Management
///
/// The caller must free the returned string with
/// [`free_cstring`](super::free_cstring).
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn show_config(config_path: *const c_char) -> FfiShowConfigResult {
    return_error_if_null_pointer!(config_path);

    let run_config =
        unwrap_or_return_error!(unsafe { resolve_run_config(config_path, ptr::null()) });
    let yaml = unwrap_or_return_error!(
        run_config
            .user
            .redacted()
            .and_then(|config| serde_yaml::to_string(&config))
            .map_err(|error| OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to serialize config: {error}"),
            ))
    );
    let yaml = unwrap_or_return_error!(to_c_string(&yaml));
    FfiShowConfigResult::ok(yaml.into_raw())
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

        // generate-key
        let generated_title = CString::new("GeneratedKey").expect("Valid CString");
        let result = unsafe {
            generate_key(
                keystore_c.as_ptr(),
                KeyType::Zk as u32,
                generated_title.as_ptr(),
            )
        };
        assert!(result.is_ok(), "Failed to generate key: {:?}", result.error);
        let key_id = unsafe { CStr::from_ptr(result.value) }
            .to_str()
            .expect("Key id should be valid UTF-8")
            .to_owned();
        assert_ne!(key_id, "");
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        // add-key
        let key_hex = CString::new("11".repeat(32)).expect("Valid CString");
        let added_title = CString::new("AddedKey").expect("Valid CString");
        let status = unsafe {
            add_key(
                keystore_c.as_ptr(),
                KeyType::Ed25519 as u32,
                key_hex.as_ptr(),
                added_title.as_ptr(),
            )
        };
        assert!(status.is_ok(), "Failed to add key: {status:?}");

        // remove-key
        let status = unsafe { remove_key(keystore_c.as_ptr(), added_title.as_ptr()) };
        assert!(status.is_ok(), "Failed to remove key: {status:?}");

        // get-peer-id
        let result = unsafe { get_peer_id(config_c.as_ptr()) };
        assert!(result.is_ok(), "Failed to get peer id: {:?}", result.error);
        let peer_id = unsafe { CStr::from_ptr(result.value) }
            .to_str()
            .expect("Peer id should be valid UTF-8")
            .to_owned();
        assert_ne!(peer_id, "");
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        // participate
        let output_dir = temp_dir.path().join("participation");
        let output_c = cstring(&output_dir);
        let external_address = CString::new("203.0.113.7").expect("Valid CString");
        let external_port = 3000u16;
        let status = unsafe {
            participate(
                config_c.as_ptr(),
                output_c.as_ptr(),
                external_address.as_ptr(),
                &raw const external_port,
            )
        };
        assert!(
            status.is_ok(),
            "Failed to generate participation data: {status:?}"
        );
        assert!(output_dir.join("participation_data.yaml").exists());
    }

    #[test]
    fn show_config_fills_in_the_defaults_and_redacts_the_secrets() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("user_config.yaml");
        let status = generate_config_sync(EmbeddedInitArgs {
            output: config_path.clone(),
            ..Default::default()
        });
        assert!(status.is_ok(), "Failed to generate config: {status:?}");
        let written = std::fs::read_to_string(&config_path).expect("Config should be readable");

        let config_c = cstring(&config_path);
        let result = unsafe { show_config(config_c.as_ptr()) };
        assert!(result.is_ok(), "Failed to show config: {:?}", result.error);
        let yaml = unsafe { CStr::from_ptr(result.value) }
            .to_str()
            .expect("Config should be valid UTF-8")
            .to_owned();
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        let config: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("Config should be YAML");
        let swarm = &config["network"]["backend"]["swarm"];
        assert_eq!(swarm["node_key"], "<secret>");
        assert_eq!(config["kms"]["backend"]["keys"], "<secret>");
        assert!(swarm["port"].is_u64());
        assert!(config["pow"]["auto_claim"]["targets"].is_sequence());
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("Config should be readable"),
            written
        );
    }
}
