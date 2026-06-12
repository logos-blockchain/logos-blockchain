use std::ffi::{CStr, CString, c_char};

use lb_key_management_system_keys::keys::{Ed25519Key, Key, UnsecuredEd25519Key, ZkKey};
use lb_node::cli::keys::{
    AddKeyArgs, GenerateKeyArgs, KeyType as NodeKeyType, RemoveKeyArgs, run_add_key, run_remove_key,
};

use crate::{
    OperationStatus, api::config::cstr_to_path, logging, result::FfiStatusResult,
    return_error_if_null_pointer,
};

/// Type of key to generate or add to a keystore.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum KeyType {
    Ed25519 = 0x0,
    Zk = 0x1,
}

impl From<KeyType> for NodeKeyType {
    fn from(value: KeyType) -> Self {
        match value {
            KeyType::Ed25519 => Self::Ed25519,
            KeyType::Zk => Self::Zk,
        }
    }
}

/// Converts a nullable C string pointer into an optional key title.
unsafe fn key_title_from_ptr(key_title: *const c_char) -> Option<String> {
    if key_title.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(key_title) }
                .to_string_lossy()
                .to_string(),
        )
    }
}

/// Result type for [`generate_key`]. On success, `value` is a pointer to a
/// NUL-terminated C string containing the new key's id.
pub type FfiGenerateKeyResult = FfiStatusResult<*mut c_char>;

/// Generates a new key and persists it to the keystore and user config files,
/// equivalent to the `generate-key` CLI command. Runs non-interactively.
///
/// # Arguments
///
/// - `user_config_path`: Path to the user config YAML file.
/// - `keystore_path`: Path to the keystore YAML file.
/// - `key_type`: The [`KeyType`] of key to generate.
/// - `key_title`: Optional (nullable) title for the new key. When null, a title
///   is auto-generated.
///
/// # Returns
///
/// A [`FfiGenerateKeyResult`] containing a pointer to an allocated C string
/// (the new key's id) on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all non-null pointers are valid NUL-terminated C strings.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn generate_key(
    user_config_path: *const c_char,
    keystore_path: *const c_char,
    key_type: KeyType,
    key_title: *const c_char,
) -> FfiGenerateKeyResult {
    return_error_if_null_pointer!("generate_key", user_config_path);
    return_error_if_null_pointer!("generate_key", keystore_path);

    let args = GenerateKeyArgs::new(
        unsafe { cstr_to_path(user_config_path) },
        unsafe { cstr_to_path(keystore_path) },
        key_type.into(),
        unsafe { key_title_from_ptr(key_title) },
        true,
    );

    let key_id = match lb_node::cli::keys::generate_key(args) {
        Ok(key_id) => key_id,
        Err(error) => {
            logging::error!("generate_key", "Error generating key: {error:?}");
            return FfiGenerateKeyResult::err(OperationStatus::ConfigurationError);
        }
    };

    match CString::new(key_id) {
        Ok(key_id) => FfiGenerateKeyResult::ok(key_id.into_raw()),
        Err(error) => {
            logging::error!("generate_key", "Failed to create CString: {error}");
            FfiGenerateKeyResult::err(OperationStatus::RuntimeError)
        }
    }
}

/// Adds an existing key to the keystore and user config files, equivalent to
/// the `add-key` CLI command. Runs non-interactively.
///
/// # Arguments
///
/// - `user_config_path`: Path to the user config YAML file.
/// - `keystore_path`: Path to the keystore YAML file.
/// - `key_type`: The [`KeyType`] of the provided key.
/// - `key_hex`: The secret key as a hex-encoded C string.
/// - `key_title`: Optional (nullable) title for the key. When null, a title is
///   auto-generated.
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
pub unsafe extern "C" fn add_key(
    user_config_path: *const c_char,
    keystore_path: *const c_char,
    key_type: KeyType,
    key_hex: *const c_char,
    key_title: *const c_char,
) -> OperationStatus {
    return_error_if_null_pointer!("add_key", user_config_path);
    return_error_if_null_pointer!("add_key", keystore_path);
    return_error_if_null_pointer!("add_key", key_hex);

    let key_hex = unsafe { CStr::from_ptr(key_hex) }.to_string_lossy();

    let key = match parse_key_hex(key_type, &key_hex) {
        Ok(key) => key,
        Err(error) => {
            logging::error!("add_key", "Invalid key: {error}");
            return OperationStatus::ValidationError;
        }
    };

    let args = AddKeyArgs::new(
        unsafe { cstr_to_path(user_config_path) },
        unsafe { cstr_to_path(keystore_path) },
        unsafe { key_title_from_ptr(key_title) },
        &key,
        true,
    );

    match run_add_key(args) {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            logging::error!("add_key", "Error adding key: {error:?}");
            OperationStatus::ConfigurationError
        }
    }
}

/// Parses a hex-encoded secret key of the given type into a [`Key`].
fn parse_key_hex(key_type: KeyType, key_hex: &str) -> Result<Key, String> {
    match key_type {
        KeyType::Ed25519 => {
            let secret_key = lb_node::config::parse_hex_ed25519_key(key_hex)?;
            let bytes: [u8; 32] = secret_key
                .as_ref()
                .try_into()
                .map_err(|_| "Invalid ed25519 secret key length".to_owned())?;
            Ok(Ed25519Key::from(UnsecuredEd25519Key::from_bytes(&bytes)).into())
        }
        KeyType::Zk => {
            let zk_key = lb_node::config::parse_hex_zk_key(key_hex)?;
            Ok(ZkKey::from(zk_key).into())
        }
    }
}

/// Removes a key with the given title from the keystore and user config
/// files, equivalent to the `remove-key` CLI command. Runs non-interactively.
///
/// # Arguments
///
/// - `user_config_path`: Path to the user config YAML file.
/// - `keystore_path`: Path to the keystore YAML file.
/// - `key_title`: Title of the key to remove.
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
pub unsafe extern "C" fn remove_key(
    user_config_path: *const c_char,
    keystore_path: *const c_char,
    key_title: *const c_char,
) -> OperationStatus {
    return_error_if_null_pointer!("remove_key", user_config_path);
    return_error_if_null_pointer!("remove_key", keystore_path);
    return_error_if_null_pointer!("remove_key", key_title);

    let key_title = unsafe { CStr::from_ptr(key_title) }
        .to_string_lossy()
        .to_string();

    let args = RemoveKeyArgs::new(
        unsafe { cstr_to_path(user_config_path) },
        unsafe { cstr_to_path(keystore_path) },
        key_title,
        true,
    );

    match run_remove_key(args) {
        Ok(()) => OperationStatus::Ok,
        Err(error) => {
            logging::error!("remove_key", "Error removing key: {error:?}");
            OperationStatus::ConfigurationError
        }
    }
}
