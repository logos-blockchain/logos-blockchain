use std::{path::PathBuf, str::FromStr};

/// Parse environment variable as `T`.
///
/// Returns `None` when variable is missing or parsing fails.
#[must_use]
pub fn env_opt<T>(key: &str) -> Option<T>
where
    T: FromStr,
{
    std::env::var(key).ok()?.parse::<T>().ok()
}

/// Parse positive environment variable as `u64`.
///
/// Returns `None` when missing, invalid, or zero.
#[must_use]
pub fn env_opt_u64(key: &str) -> Option<u64> {
    env_opt::<u64>(key).filter(|value| *value > 0)
}

/// Parse positive environment variable as `u64` with fallback default.
#[must_use]
pub fn env_u64(key: &str, default: u64) -> u64 {
    env_opt_u64(key).unwrap_or(default)
}

/// Parse boolean-like environment variable.
///
/// Accepted truthy values: `1`, `true`, `yes`, `on` (case-insensitive).
/// Missing or any other value resolves to `false`.
#[must_use]
pub fn env_flag(key: &str) -> bool {
    let Ok(raw) = std::env::var(key) else {
        return false;
    };

    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[must_use]
pub fn debug_tracing() -> bool {
    env_flag("LOGOS_BLOCKCHAIN_TESTS_TRACING")
}

#[must_use]
pub fn logos_blockchain_cfgsync_port() -> Option<u16> {
    env_opt("LOGOS_BLOCKCHAIN_CFGSYNC_PORT")
}

#[must_use]
pub fn logos_blockchain_log_dir() -> Option<PathBuf> {
    std::env::var("LOGOS_BLOCKCHAIN_LOG_DIR")
        .ok()
        .map(PathBuf::from)
}

#[must_use]
pub fn log_level() -> Option<String> {
    std::env::var("LOG_LEVEL").ok()
}

#[must_use]
pub fn logos_blockchain_testnet_image() -> Option<String> {
    std::env::var("LOGOS_BLOCKCHAIN_TESTNET_IMAGE").ok()
}

#[must_use]
pub fn logos_blockchain_testnet_image_pull_policy() -> Option<String> {
    std::env::var("LOGOS_BLOCKCHAIN_TESTNET_IMAGE_PULL_POLICY").ok()
}

#[must_use]
pub fn rust_log() -> Option<String> {
    std::env::var("RUST_LOG").ok()
}

#[must_use]
pub fn lb_time_service_backend() -> Option<String> {
    std::env::var("LOGOS_BLOCKCHAIN_TIME_BACKEND").ok()
}

#[must_use]
pub fn logos_blockchain_system_monitor_enabled() -> bool {
    std::env::var("LOGOS_BLOCKCHAIN_SYSTEM_MONITOR").map_or(true, |raw| {
        !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

#[must_use]
pub fn logos_blockchain_system_monitor_interval_secs() -> u64 {
    env_u64("LOGOS_BLOCKCHAIN_SYSTEM_MONITOR_INTERVAL_SECS", 10)
}

/// Set an environment variable to a default value if it is not already set.
pub fn set_default_env(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: Used as an early-run default. Prefer setting env vars in the
        // shell for multi-threaded runs.
        unsafe {
            std::env::set_var(key, value);
        }
    }
}

/// Replace an environment variable to a default value, returning the current
/// optional value if already set or Noneotherwise.
#[must_use]
pub fn replace_default_env(key: &str, value: &str) -> Option<String> {
    let current = std::env::var(key).ok();
    // SAFETY: Used as an early-run default. Prefer setting env vars in the
    // shell for multi-threaded runs.
    unsafe { std::env::set_var(key, value) };
    current
}

/// Remove an environment variable if set.
pub fn remove_default_env(key: &str) {
    if std::env::var_os(key).is_some() {
        // SAFETY: Used as an early-run default. Prefer setting env vars in the
        // shell for multi-threaded runs.
        unsafe {
            std::env::remove_var(key);
        }
    }
}
