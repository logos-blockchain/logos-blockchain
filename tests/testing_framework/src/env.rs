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

/// Parse positive environment variable as `u32`.
///
/// Returns `None` when missing, invalid, zero, or out of `u32` range.
#[must_use]
pub fn env_opt_u32(key: &str) -> Option<u32> {
    let value = env_opt_u64(key)?;
    u32::try_from(value).ok()
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
pub fn nomos_cfgsync_port() -> Option<u16> {
    env_opt("LOGOS_BLOCKCHAIN_CFGSYNC_PORT")
}

#[must_use]
pub fn nomos_log_dir() -> Option<PathBuf> {
    std::env::var("LOGOS_BLOCKCHAIN_LOG_DIR")
        .ok()
        .map(PathBuf::from)
}

#[must_use]
pub fn nomos_log_level() -> Option<String> {
    std::env::var("LOGOS_BLOCKCHAIN_LOG_LEVEL").ok()
}

#[must_use]
pub fn nomos_testnet_image() -> Option<String> {
    std::env::var("LOGOS_BLOCKCHAIN_TESTNET_IMAGE").ok()
}

#[must_use]
pub fn nomos_testnet_image_pull_policy() -> Option<String> {
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
